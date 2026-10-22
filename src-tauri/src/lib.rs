pub mod api;
pub mod config;
#[cfg(feature = "desktop")]
pub mod desktop;
pub mod download;
pub mod engine;
pub mod error;

use config::{ModelKind, ModelSpec, Paths, Settings};
use download::DownloadManager;
use engine::EngineManager;
use error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{Mutex, RwLock};

pub struct AppCore {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    pub engine: Arc<EngineManager>,
    pub downloads: Arc<DownloadManager>,
    pub settings_gate: Mutex<()>,
    pub(crate) library_gate: Mutex<()>,
    service: Mutex<Option<api::ApiServer>>,
    startup_error: RwLock<Option<AppError>>,
    shutting_down: AtomicBool,
}
impl AppCore {
    pub async fn new(paths: Paths) -> Result<Arc<Self>> {
        paths.prepare().await?;
        let (settings, startup_error) = match config::load_settings(&paths).await {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(e)),
        };
        let downloads = DownloadManager::new(paths.clone());
        downloads.restore(&settings.models).await;
        Ok(Arc::new(Self {
            engine: EngineManager::new(paths.runtime.clone()),
            downloads,
            paths,
            settings: RwLock::new(settings),
            settings_gate: Mutex::new(()),
            library_gate: Mutex::new(()),
            service: Mutex::new(None),
            startup_error: RwLock::new(startup_error),
            shutting_down: AtomicBool::new(false),
        }))
    }
    pub fn ensure_running(&self) -> Result<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            Err(AppError::new("shutting_down", "应用正在退出。"))
        } else {
            Ok(())
        }
    }
    pub async fn snapshot(&self) -> Value {
        let settings = self.settings.read().await.clone();
        let mut inventory = vec![];
        for model in &settings.models {
            let path = model.resolved_path(&self.paths);
            let metadata = tokio::fs::metadata(&path)
                .await
                .ok()
                .filter(|m| m.is_file());
            inventory.push(json!({"id":model.id,"path":path.to_string_lossy(),"size":metadata.as_ref().map(|m|m.len()).unwrap_or(0),"exists":metadata.is_some(),"managed":model.path.is_empty()}));
        }
        let service = self.service.lock().await;
        let service_state = json!({"running": service.as_ref().is_some_and(api::ApiServer::is_running), "address":service.as_ref().map(|s|s.address.clone()).unwrap_or_else(||service_url(&settings.service))});
        drop(service);
        json!({"settings":settings,"engine":self.engine.snapshot().await,"downloads":self.downloads.snapshot().await,
            "service":service_state,"hf":self.downloads.credential_status().await,"inventory":inventory,
            "paths":{"models":self.paths.models.to_string_lossy(),"logs":self.paths.logs.to_string_lossy()},
            "version":env!("CARGO_PKG_VERSION"),"dev_mode":cfg!(debug_assertions),"startup_error":*self.startup_error.read().await})
    }
    pub async fn model(&self, id: &str) -> Result<ModelSpec> {
        self.settings
            .read()
            .await
            .models
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| AppError::new("model_not_found", "模型配置不存在。"))
    }
    pub async fn validate_model_changes(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let old = self.settings.read().await.clone();
        if settings.models.len() != old.models.len()
            || old
                .models
                .iter()
                .any(|m| !settings.models.iter().any(|n| n.id == m.id))
        {
            return Err(AppError::invalid("请通过模型管理添加或移除模型配置。"));
        }
        for model in &old.models {
            let next = settings
                .models
                .iter()
                .find(|m| m.id == model.id)
                .ok_or_else(|| AppError::invalid("模型配置无效。"))?;
            if serde_json::to_value(model).ok() != serde_json::to_value(next).ok()
                && (self.model_active(model).await || self.download_active(&model.id).await)
            {
                return Err(AppError::new(
                    "model_busy",
                    "修改模型配置前请先卸载模型并取消下载。",
                ));
            }
        }
        Ok(())
    }
    pub async fn persist_settings(&self, settings: Settings) -> Result<Settings> {
        self.ensure_running()?;
        settings.validate()?;
        config::save_settings(&self.paths, &settings).await?;
        *self.settings.write().await = settings.clone();
        *self.startup_error.write().await = None;
        tracing::info!(event = "settings_saved");
        Ok(settings)
    }
    pub async fn load(&self, id: &str) -> Result<()> {
        self.ensure_running()?;
        let _guard = self.library_gate.lock().await;
        self.ensure_running()?;
        let mut model = self.model(id).await?;
        if self.download_active(id).await {
            return Err(AppError::new(
                "model_busy",
                "模型正在下载或校验，请等待完成。",
            ));
        }
        model.path = model
            .resolved_path(&self.paths)
            .to_string_lossy()
            .into_owned();
        let params = self.settings.read().await.inference.clone();
        self.engine.load(model, params).await
    }
    async fn download_active(&self, id: &str) -> bool {
        self.downloads
            .snapshot()
            .await
            .as_array()
            .is_some_and(|list| {
                list.iter().any(|d| {
                    d["id"] == id
                        && matches!(
                            d["status"].as_str(),
                            Some("downloading" | "verifying" | "queued" | "resolving")
                        )
                })
            })
    }
    async fn model_active(&self, model: &ModelSpec) -> bool {
        let snapshot = self.engine.snapshot().await;
        let slot = &snapshot[model.kind.to_string()];
        matches!(
            slot["status"].as_str(),
            Some("loaded" | "ready" | "loading" | "unloading")
        ) && (slot["model"]["id"] == model.id
            || slot["model"] == model.name
            || slot["model_id"] == model.id
            || slot["status"] == "loading")
    }
    pub async fn download(&self, id: &str) -> Result<()> {
        self.ensure_running()?;
        let _guard = self.library_gate.lock().await;
        let model = self.model(id).await?;
        if self.model_active(&model).await {
            return Err(AppError::new("model_busy", "请先卸载此模型再下载。"));
        }
        self.downloads.start(model).await
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.ensure_running()?;
        let _guard = self.library_gate.lock().await;
        let model = self.model(id).await?;
        if self.model_active(&model).await || self.download_active(id).await {
            return Err(AppError::new(
                "model_busy",
                "请先取消下载并卸载模型，再删除文件。",
            ));
        }
        self.downloads.delete(&model).await
    }
    pub async fn add_model(&self, model: ModelSpec) -> Result<()> {
        self.ensure_running()?;
        let _guard = self.settings_gate.lock().await;
        let mut settings = self.settings.read().await.clone();
        settings.models.push(model);
        self.persist_settings(settings).await?;
        Ok(())
    }
    pub async fn remove_model(&self, id: &str) -> Result<()> {
        self.ensure_running()?;
        let _library = self.library_gate.lock().await;
        let _settings = self.settings_gate.lock().await;
        if matches!(id, "qwen3-generation" | "qwen3-embedding") {
            return Err(AppError::invalid(
                "默认模型配置不可移除，可以卸载或删除本地文件。",
            ));
        }
        let model = self.model(id).await?;
        if self.model_active(&model).await || self.download_active(id).await {
            return Err(AppError::new(
                "model_busy",
                "模型使用中，请先卸载并取消下载。",
            ));
        }
        let mut settings = self.settings.read().await.clone();
        settings.models.retain(|m| m.id != id);
        self.persist_settings(settings).await?;
        Ok(())
    }
    pub async fn service_action(&self, action: &str) -> Result<()> {
        self.ensure_running()?;
        if !matches!(action, "start" | "stop" | "restart") {
            return Err(AppError::invalid("未知服务操作。"));
        }
        let mut service = self.service.lock().await;
        if service.as_ref().is_some_and(|server| !server.is_running()) {
            service.take();
        }
        self.ensure_running()?;
        if matches!(action, "stop" | "restart") {
            if let Some(mut current) = service.take() {
                current.stop().await;
            }
        }
        if matches!(action, "start" | "restart") && service.is_none() {
            let settings = self.settings.read().await.service.clone();
            *service = Some(api::start(self.engine.clone(), settings).await?);
        }
        tracing::info!(event = "service_changed", action);
        Ok(())
    }
    pub async fn auto_start(&self) {
        let settings = self.settings.read().await.clone();
        if !settings.service.auto_start {
            return;
        }
        if let Err(e) = self.service_action("start").await {
            *self.startup_error.write().await = Some(e);
            return;
        }
        for kind in [ModelKind::Generation, ModelKind::Embedding] {
            if self.ensure_running().is_err() {
                return;
            }
            if let Some(model) = settings
                .models
                .iter()
                .find(|m| m.kind == kind && m.resolved_path(&self.paths).is_file())
            {
                if let Err(e) = self.load(&model.id).await {
                    tracing::warn!(event="autoload_failed",code=%e.code);
                    self.startup_error.write().await.get_or_insert(e);
                }
            }
        }
    }
    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(mut service) = self.service.lock().await.take() {
            service.stop().await;
        }
        tokio::join!(self.downloads.shutdown(), self.engine.shutdown());
        tracing::info!(event = "shutdown_complete");
    }
}
fn service_url(s: &config::ServiceSettings) -> String {
    if s.host.contains(':') {
        format!("http://[{}]:{}", s.host, s.port)
    } else {
        format!("http://{}:{}", s.host, s.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{net::TcpListener, time::timeout};

    const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

    async fn startup_core() -> (tempfile::TempDir, Arc<AppCore>) {
        let tmp = tempfile::tempdir().unwrap();
        let mut paths = Paths::new(tmp.path().join("data"), tmp.path().join("runtime"));
        paths.hf_candidates.clear();
        let core = AppCore::new(paths).await.unwrap();
        // Reserve an OS-selected port until the isolated configuration is saved.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut settings = core.settings.read().await.clone();
        settings.service.port = listener.local_addr().unwrap().port();
        settings.inference.gpu_layers = 0;
        core.persist_settings(settings).await.unwrap();
        (tmp, core)
    }

    async fn local_fixture(core: &AppCore, id: &str, bytes: &[u8]) {
        let path = core.model(id).await.unwrap().resolved_path(&core.paths);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(path, bytes).await.unwrap();
    }

    fn gguf_header() -> [u8; 24] {
        let mut header = [0; 24];
        header[..4].copy_from_slice(b"GGUF");
        header[4..8].copy_from_slice(&3u32.to_le_bytes());
        header
    }

    async fn enable_startup(core: &AppCore) {
        let mut settings = core.settings.read().await.clone();
        settings.service.auto_start = true;
        core.persist_settings(settings).await.unwrap();
    }

    async fn assert_healthy(core: &AppCore) {
        let address = core
            .service
            .lock()
            .await
            .as_ref()
            .expect("API 服务应已启动")
            .address
            .clone();
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(STARTUP_TIMEOUT)
            .build()
            .unwrap();
        let response = client
            .get(format!("{address}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"status": "ok", "service": "Qwen Desk"})
        );
    }

    #[tokio::test]
    async fn disabled_startup_leaves_service_and_local_models_untouched() {
        let (_tmp, core) = startup_core().await;
        for id in ["qwen3-generation", "qwen3-embedding"] {
            local_fixture(&core, id, &gguf_header()).await;
        }
        let before = core.engine.snapshot().await;
        timeout(STARTUP_TIMEOUT, core.auto_start()).await.unwrap();
        assert!(core.service.lock().await.is_none());
        assert_eq!(core.engine.snapshot().await, before);
        assert!(core.startup_error.read().await.is_none());
        assert_eq!(core.downloads.snapshot().await, json!([]));
        core.shutdown().await;
    }

    #[tokio::test]
    async fn persisted_startup_serves_health_without_local_models() {
        let (_tmp, original) = startup_core().await;
        enable_startup(&original).await;
        let paths = original.paths.clone();
        original.shutdown().await;
        drop(original);
        let core = AppCore::new(paths).await.unwrap();
        assert!(core.settings.read().await.service.auto_start);
        let before = core.engine.snapshot().await;
        timeout(STARTUP_TIMEOUT, core.auto_start()).await.unwrap();
        assert_healthy(&core).await;
        assert_eq!(core.engine.snapshot().await, before);
        assert!(core.startup_error.read().await.is_none());
        assert_eq!(core.downloads.snapshot().await, json!([]));
        for model in &core.settings.read().await.models {
            assert!(!model.resolved_path(&core.paths).exists());
        }
        core.shutdown().await;
    }

    #[tokio::test]
    async fn startup_port_collision_is_visible_and_skips_autoload() {
        let (_tmp, core) = startup_core().await;
        enable_startup(&core).await;
        local_fixture(&core, "qwen3-generation", &gguf_header()).await;
        let port = core.settings.read().await.service.port;
        let _occupied = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let before = core.engine.snapshot().await;
        timeout(STARTUP_TIMEOUT, core.auto_start()).await.unwrap();
        let snapshot = core.snapshot().await;
        assert_eq!(snapshot["startup_error"]["code"], "port_unavailable");
        assert_eq!(snapshot["service"]["running"], false);
        assert_eq!(snapshot["engine"], before);
        core.shutdown().await;
    }

    #[tokio::test]
    async fn startup_serves_health_while_autoload_waits_then_attempts_both_kinds() {
        let (_tmp, core) = startup_core().await;
        enable_startup(&core).await;
        for id in ["qwen3-generation", "qwen3-embedding"] {
            local_fixture(&core, id, &gguf_header()).await;
        }
        // Missing files before existing fixtures must not prevent selection.
        let mut settings = core.settings.read().await.clone();
        for mut missing in settings.models.clone() {
            missing.id = format!("missing-{}", missing.kind);
            missing.name = missing.id.clone();
            settings.models.insert(0, missing);
        }
        core.persist_settings(settings).await.unwrap();
        let before = core.engine.snapshot().await;
        let gate = core.library_gate.lock().await;
        let mut startup = Box::pin(core.auto_start());
        timeout(STARTUP_TIMEOUT, async {
            loop {
                assert!(futures_util::poll!(startup.as_mut()).is_pending());
                if core.service.lock().await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_healthy(&core).await;
        assert_eq!(core.engine.snapshot().await, before);
        assert!(futures_util::poll!(startup.as_mut()).is_pending());
        drop(gate);
        timeout(STARTUP_TIMEOUT, startup).await.unwrap();
        let snapshot = core.snapshot().await;
        for kind in ["generation", "embedding"] {
            assert_eq!(snapshot["engine"][kind]["status"], "error");
            assert_eq!(snapshot["engine"][kind]["error"]["code"], "runtime_missing");
            assert_eq!(
                snapshot["engine"][kind]["model"]["id"],
                format!("qwen3-{kind}")
            );
        }
        assert_eq!(snapshot["startup_error"]["code"], "runtime_missing");
        assert_eq!(core.downloads.snapshot().await, json!([]));
        assert_healthy(&core).await;
        core.shutdown().await;
    }

    #[tokio::test]
    async fn corrupt_autoload_is_visible_and_other_kind_still_attempts_loading() {
        let (_tmp, core) = startup_core().await;
        enable_startup(&core).await;
        local_fixture(&core, "qwen3-generation", b"invalid").await;
        local_fixture(&core, "qwen3-embedding", &gguf_header()).await;
        timeout(STARTUP_TIMEOUT, core.auto_start()).await.unwrap();
        assert_healthy(&core).await;
        let snapshot = core.snapshot().await;
        assert_eq!(snapshot["startup_error"]["code"], "model_corrupt");
        assert_eq!(
            snapshot["startup_error"]["message"],
            "模型文件不完整，请重新下载。"
        );
        assert_eq!(snapshot["engine"]["generation"]["status"], "unloaded");
        assert_eq!(snapshot["engine"]["embedding"]["status"], "error");
        assert_eq!(
            snapshot["engine"]["embedding"]["error"]["code"],
            "runtime_missing"
        );
        core.shutdown().await;
    }

    #[tokio::test]
    async fn delayed_auto_start_cannot_reopen_listener_after_shutdown() {
        let (_tmp, core) = startup_core().await;
        enable_startup(&core).await;
        let port = core.settings.read().await.service.port;
        let gate = core.service.lock().await;
        let mut startup = Box::pin(core.auto_start());
        assert!(futures_util::poll!(startup.as_mut()).is_pending());
        let mut shutdown = Box::pin(core.shutdown());
        assert!(futures_util::poll!(shutdown.as_mut()).is_pending());
        drop(gate);
        timeout(STARTUP_TIMEOUT, startup).await.unwrap();
        timeout(STARTUP_TIMEOUT, shutdown).await.unwrap();
        timeout(STARTUP_TIMEOUT, core.auto_start()).await.unwrap();
        assert!(core.service.lock().await.is_none());
        assert!(TcpListener::bind(("127.0.0.1", port)).await.is_ok());
    }

    #[tokio::test]
    async fn model_library_persists_and_rejects_duplicate_registration() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("data"), tmp.path().join("runtime"));
        let core = AppCore::new(paths.clone()).await.unwrap();
        let mut model = core.model("qwen3-generation").await.unwrap();
        model.id = "custom-model".into();
        model.name = "my-model".into();
        core.add_model(model.clone()).await.unwrap();
        assert!(core.add_model(model).await.is_err());
        assert_eq!(config::load_settings(&paths).await.unwrap().models.len(), 3);
        let mut next = core.settings.read().await.clone();
        next.models[2].name = "renamed-model".into();
        core.validate_model_changes(&next).await.unwrap();
        next.models.pop();
        assert!(core.validate_model_changes(&next).await.is_err());
        assert!(core.remove_model("qwen3-generation").await.is_err());
        core.remove_model("custom-model").await.unwrap();
        assert_eq!(config::load_settings(&paths).await.unwrap().models.len(), 2);
        core.shutdown().await;
        core.shutdown().await;
        assert!(core
            .add_model(Settings::default().models.remove(0))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn queued_service_start_cannot_outlive_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::new(tmp.path().join("data"), tmp.path().join("runtime"));
        let core = AppCore::new(paths).await.unwrap();
        let gate = core.service.lock().await;
        let mut start = Box::pin(core.service_action("start"));
        assert!(futures_util::poll!(start.as_mut()).is_pending());
        let mut shutdown = Box::pin(core.shutdown());
        assert!(futures_util::poll!(shutdown.as_mut()).is_pending());
        drop(gate);
        assert_eq!(start.await.unwrap_err().code, "shutting_down");
        shutdown.await;
        assert!(core.service.lock().await.is_none());
    }
}
