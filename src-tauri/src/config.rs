use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Generation,
    Embedding,
}
impl std::fmt::Display for ModelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Generation => "generation",
            Self::Embedding => "embedding",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub id: String,
    pub name: String,
    pub kind: ModelKind,
    #[serde(default)]
    pub source_repo: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub compatibility: String,
}
impl ModelSpec {
    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return Err(AppError::invalid(
                "模型 ID 只能包含英文字母、数字、下划线及短横线（1–64 字符）。",
            ));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 200
            || self.name.chars().any(char::is_control)
        {
            return Err(AppError::invalid(
                "模型名称不能为空或包含控制字符，最多 200 字节。",
            ));
        }
        if self.compatibility.len() > 2048 {
            return Err(AppError::invalid("兼容性说明过长。"));
        }
        if self.path.is_empty() {
            if !valid_repo(&self.source_repo) || !valid_filename(&self.filename) {
                return Err(AppError::invalid(
                    "请输入 owner/repository 和仓库内的 GGUF 文件名，不允许路径穿越。",
                ));
            }
        } else {
            let path = Path::new(&self.path);
            if self.path.len() > 4096
                || self.path.chars().any(char::is_control)
                || !path.is_absolute()
                || !is_gguf(path)
            {
                return Err(AppError::invalid(
                    "本地模型须为绝对路径的 .gguf 文件。AWQ/Safetensors 文件不能直接加载。",
                ));
            }
            #[cfg(windows)]
            if self.path.starts_with("\\\\") || self.path.chars().filter(|c| *c == ':').count() != 1
            {
                return Err(AppError::invalid(
                    "本地模型仅支持本机磁盘路径，不支持设备路径、网络共享或备用数据流。",
                ));
            }
        }
        Ok(())
    }
    pub fn resolved_path(&self, paths: &Paths) -> PathBuf {
        if self.path.is_empty() {
            paths
                .models
                .join(&self.id)
                .join(self.filename.rsplit('/').next().unwrap_or("model.gguf"))
        } else {
            PathBuf::from(&self.path)
        }
    }
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !reserved_windows_name(s)
}
fn reserved_windows_name(s: &str) -> bool {
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
pub fn valid_repo(s: &str) -> bool {
    let parts: Vec<_> = s.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.len() <= 96
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
}
pub fn valid_filename(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 512
        && s.to_ascii_lowercase().ends_with(".gguf")
        && s.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && !p.ends_with(['.', ' '])
                && !reserved_windows_name(p)
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        })
}
pub fn is_gguf(p: &Path) -> bool {
    p.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceSettings {
    pub context_size: u32,
    pub batch_size: u32,
    pub threads: u32,
    pub gpu_layers: i32,
    pub gpu_memory_mb: u32,
    pub cache_type: String,
}
impl Default for InferenceSettings {
    fn default() -> Self {
        Self {
            context_size: 4096,
            batch_size: 512,
            threads: std::thread::available_parallelism()
                .map(|n| n.get() as u32)
                .unwrap_or(4)
                .min(8),
            gpu_layers: -1,
            gpu_memory_mb: 1024,
            cache_type: "f16".into(),
        }
    }
}
impl InferenceSettings {
    pub fn validate(&self) -> Result<()> {
        if !(512..=131072).contains(&self.context_size)
            || !(32..=4096).contains(&self.batch_size)
            || self.batch_size > self.context_size
            || !(1..=256).contains(&self.threads)
            || !(-1..=1000).contains(&self.gpu_layers)
            || self.gpu_memory_mb > 65536
            || !matches!(self.cache_type.as_str(), "f16" | "q8_0" | "q4_0")
        {
            return Err(AppError::invalid("推理参数超出范围：上下文 512–131072，批量 32–4096 且不超过上下文，线程 1–256，GPU 层 -1–1000，显存预留 0–65536 MiB。"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSettings {
    pub host: String,
    pub port: u16,
    pub concurrency: usize,
    pub timeout_secs: u64,
    #[serde(default)]
    pub auto_start: bool,
}
impl Default for ServiceSettings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8848,
            concurrency: 2,
            timeout_secs: 120,
            auto_start: false,
        }
    }
}
impl ServiceSettings {
    pub fn validate(&self) -> Result<()> {
        if self.host.parse::<IpAddr>().is_err()
            || self.port == 0
            || !(1..=16).contains(&self.concurrency)
            || !(5..=1800).contains(&self.timeout_secs)
        {
            return Err(AppError::invalid(
                "监听地址必须是有效 IP，端口 1–65535，并发 1–16，超时 5–1800 秒。",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    #[serde(default = "default_language")]
    pub language: String,
    pub autostart: bool,
    pub silent_start: bool,
    pub close_to_tray: bool,
    pub tray_enabled: bool,
    pub log_level: String,
}
fn default_language() -> String {
    "zh-CN".into()
}
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            autostart: false,
            silent_start: false,
            close_to_tray: true,
            tray_enabled: true,
            log_level: "info".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub models: Vec<ModelSpec>,
    pub inference: InferenceSettings,
    pub service: ServiceSettings,
    pub app: AppSettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            models: vec![
                ModelSpec {
                    id: "qwen3-generation".into(),
                    name: "warshanks/Qwen3-4B-Instruct-2507-AWQ".into(),
                    kind: ModelKind::Generation,
                    source_repo: "unsloth/Qwen3-4B-Instruct-2507-GGUF".into(),
                    filename: "Qwen3-4B-Instruct-2507-Q4_K_M.gguf".into(),
                    path: String::new(),
                    compatibility:
                        "同源 Qwen3-4B-Instruct-2507 的 Q4_K_M GGUF 替代量化；不直接运行 AWQ 权重。"
                            .into(),
                },
                ModelSpec {
                    id: "qwen3-embedding".into(),
                    name: "Qwen/Qwen3-Embedding-0.6B".into(),
                    kind: ModelKind::Embedding,
                    source_repo: "Qwen/Qwen3-Embedding-0.6B-GGUF".into(),
                    filename: "Qwen3-Embedding-0.6B-Q8_0.gguf".into(),
                    path: String::new(),
                    compatibility: "官方 Q8_0 GGUF，last-token pooling，L2 归一化向量。".into(),
                },
            ],
            inference: InferenceSettings::default(),
            service: ServiceSettings::default(),
            app: AppSettings::default(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        self.inference.validate()?;
        self.service.validate()?;
        if !matches!(self.app.language.as_str(), "zh-CN" | "en") {
            return Err(AppError::invalid("Unsupported interface language."));
        }
        if self.models.len() > 100 {
            return Err(AppError::invalid("最多管理 100 个模型。"));
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for model in &self.models {
            model.validate()?;
            if !ids.insert(model.id.to_ascii_lowercase()) || !names.insert(model.name.clone()) {
                return Err(AppError::invalid("模型 ID 和 API 名称不得重复。"));
            }
        }
        if !self.app.tray_enabled && (self.app.close_to_tray || self.app.silent_start) {
            return Err(AppError::invalid("隐藏到托盘或静默启动需要启用系统托盘。"));
        }
        if !matches!(
            self.app.log_level.as_str(),
            "error" | "warn" | "info" | "debug"
        ) {
            return Err(AppError::invalid("日志级别无效。"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub models: PathBuf,
    pub logs: PathBuf,
    pub runtime: PathBuf,
    pub config: PathBuf,
    pub hf_candidates: Vec<PathBuf>,
}
impl Paths {
    pub fn new(data: PathBuf, runtime: PathBuf) -> Self {
        // Settings entered in the app must override legacy credential files.
        let mut hf_candidates = vec![data.join("huggingface.yml")];
        if let Ok(cwd) = std::env::current_dir() {
            hf_candidates.push(cwd.join("huggingface.yml"));
        }
        #[cfg(debug_assertions)]
        if let Some(project) = Path::new(env!("CARGO_MANIFEST_DIR")).parent() {
            hf_candidates.push(project.join("huggingface.yml"));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                hf_candidates.push(dir.join("huggingface.yml"));
            }
        }
        hf_candidates.dedup();
        Self {
            models: data.join("models"),
            logs: data.join("logs"),
            config: data.join("settings.json"),
            data,
            runtime,
            hf_candidates,
        }
    }
    pub async fn prepare(&self) -> Result<()> {
        for dir in [&self.data, &self.models, &self.logs] {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|_| AppError::io())?;
        }
        Ok(())
    }
}
pub async fn load_settings(paths: &Paths) -> Result<Settings> {
    match tokio::fs::read(&paths.config).await {
        Ok(bytes) => {
            if bytes.len() > 1024 * 1024 {
                return Err(AppError::new(
                    "config_invalid",
                    "配置文件过大。请恢复有效的 settings.json。",
                ));
            }
            let settings: Settings = serde_json::from_slice(&bytes).map_err(|_| {
                AppError::new(
                    "config_invalid",
                    "配置文件格式错误。原文件已保留，请修复 settings.json 后重启。",
                )
            })?;
            settings.validate()?;
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(_) => Err(AppError::new(
            "config_read_failed",
            "无法读取配置文件，请检查应用数据目录权限。",
        )),
    }
}
pub async fn save_settings(paths: &Paths, settings: &Settings) -> Result<()> {
    settings.validate()?;
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|_| AppError::new("config_invalid", "无法保存配置。"))?;
    let temp = paths.data.join("settings.json.new");
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(&temp)
        .await
        .map_err(|_| AppError::io())?;
    file.write_all(&bytes).await.map_err(|_| AppError::io())?;
    file.sync_all().await.map_err(|_| AppError::io())?;
    drop(file);
    tokio::fs::rename(&temp, &paths.config)
        .await
        .map_err(|_| AppError::io())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_valid() {
        Settings::default().validate().unwrap();
    }
    #[test]
    fn language_migration_and_validation() {
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value["app"].as_object_mut().unwrap().remove("language");
        let mut settings: Settings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.app.language, "zh-CN");
        settings.app.language = "en".into();
        settings.validate().unwrap();
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
        assert_eq!(restored.app.language, "en");
        settings.app.language = "invalid".into();
        assert!(settings.validate().is_err());
    }
    #[test]
    fn startup_setting_is_backward_compatible() {
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value["service"]
            .as_object_mut()
            .unwrap()
            .remove("auto_start");
        let settings: Settings = serde_json::from_value(value).unwrap();
        settings.validate().unwrap();
        assert!(!settings.service.auto_start);
        for enabled in [true, false] {
            let mut settings = settings.clone();
            settings.service.auto_start = enabled;
            let restored: Settings =
                serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
            assert_eq!(restored.service.auto_start, enabled);
        }
    }
    #[test]
    fn rejects_path_traversal_and_windows_devices() {
        for p in [
            "../file.gguf",
            "x/../file.gguf",
            "C:/file.gguf",
            "x\\file.gguf",
            "/file.gguf",
            "NUL.gguf",
            "a//file.gguf",
            "file.gguf:secret",
            "a./file.gguf",
        ] {
            assert!(!valid_filename(p), "{p}");
        }
        assert!(valid_filename("weights/model-Q4_K_M.gguf"));
        for id in ["..", "../a", "CON", "nul", "LPT1", ""] {
            assert!(!valid_id(id));
        }
    }
    #[test]
    fn prevents_unreachable_silent_window() {
        let mut s = Settings::default();
        s.app.tray_enabled = false;
        assert!(s.validate().is_err());
        s.app.close_to_tray = false;
        s.validate().unwrap();
        s.app.silent_start = true;
        assert!(s.validate().is_err());
    }
    #[test]
    fn validates_bounds_and_duplicate_ids() {
        let mut s = Settings::default();
        s.models.push(s.models[0].clone());
        assert!(s.validate().is_err());
        s.models.pop();
        s.service.host = "localhost/secret".into();
        assert!(s.validate().is_err());
        s.service.host = "::1".into();
        s.service.port = 0;
        assert!(s.validate().is_err());
    }
    #[tokio::test]
    async fn durable_roundtrip_and_corrupt_preservation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().into(), dir.path().into());
        paths.prepare().await.unwrap();
        let mut s = Settings::default();
        s.service.port = 9123;
        save_settings(&paths, &s).await.unwrap();
        assert_eq!(load_settings(&paths).await.unwrap().service.port, 9123);
        s.service.port = 9124;
        save_settings(&paths, &s).await.unwrap();
        assert_eq!(load_settings(&paths).await.unwrap().service.port, 9124);
        tokio::fs::write(&paths.config, b"broken").await.unwrap();
        assert!(load_settings(&paths).await.is_err());
        assert_eq!(tokio::fs::read(&paths.config).await.unwrap(), b"broken");
    }
}
