//! Authenticated HF transfers. Credentials and remote response bodies never enter errors or IPC.
use crate::{
    config::{valid_filename, valid_id, valid_repo, ModelSpec, Paths},
    error::{AppError, Result},
};
use futures_util::StreamExt;
use reqwest::{
    header::{HeaderValue, AUTHORIZATION, CONTENT_ENCODING, CONTENT_RANGE, RANGE},
    StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const MAX_METADATA: usize = 16 * 1024 * 1024;
const MAX_MODEL: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Serialize)]
struct Record {
    id: String,
    status: String,
    downloaded: u64,
    total: Option<u64>,
    error: Option<String>,
    path: Option<String>,
}
struct Job {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
#[derive(Default)]
struct Control {
    closed: bool,
    jobs: BTreeMap<String, Job>,
}
pub struct DownloadManager {
    paths: Paths,
    client: std::result::Result<reqwest::Client, ()>,
    control: Mutex<Control>,
    records: Mutex<BTreeMap<String, Record>>,
    slots: Semaphore,
    credential_gate: Mutex<()>,
}

impl DownloadManager {
    pub fn new(paths: Paths) -> Arc<Self> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(24 * 60 * 60))
            // reqwest also removes sensitive headers on cross-host redirects.
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent("QwenDesk/1.0")
            .build()
            .map_err(|_| ());
        Arc::new(Self {
            paths,
            client,
            control: Mutex::new(Control::default()),
            records: Mutex::new(BTreeMap::new()),
            slots: Semaphore::new(2),
            credential_gate: Mutex::new(()),
        })
    }

    pub async fn credential_status(&self) -> Value {
        match credentials(&self.paths.hf_candidates).await {
            Ok(_) => {
                json!({"status":"ready","message":"已找到本地 Hugging Face 凭据；下载时验证访问权限。"})
            }
            Err(e) => {
                json!({"status":if e.code == "credential_missing" {"missing"} else {"invalid"},"message":e.message})
            }
        }
    }

    /// Write-only IPC: the token is validated and stored separately from settings.
    /// Neither snapshots nor return values contain the credential.
    pub async fn save_credential(&self, token: &str) -> Result<()> {
        let _gate = self.credential_gate.lock().await;
        let bytes = serde_yaml::to_string(&json!({"token": token.trim()}))
            .map_err(|_| credential_invalid())?;
        parse_credentials(bytes.as_bytes())?;
        let target = self.paths.data.join("huggingface.yml");
        let temp = tempfile_credential_path(&self.paths.data);
        let result = async {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp).await?;
            file.write_all(bytes.as_bytes()).await?;
            file.sync_all().await?;
            drop(file);
            tokio::fs::rename(&temp, &target).await
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(AppError::new(
                "credential_save_failed",
                "无法保存 Hugging Face 令牌，请检查应用数据目录权限。",
            ));
        }
        Ok(())
    }

    pub async fn start(self: &Arc<Self>, model: ModelSpec) -> Result<()> {
        validate_model(&model)?;
        let key = model.id.to_ascii_lowercase();
        let mut control = self.control.lock().await;
        if control.closed {
            return Err(AppError::new("shutdown", "应用正在退出。"));
        }
        if control
            .jobs
            .get(&key)
            .is_some_and(|j| !j.task.is_finished())
        {
            return Err(AppError::new("download_busy", "该模型已有下载任务。"));
        }
        control.jobs.retain(|_, j| !j.task.is_finished());
        if control.jobs.len() >= 100 {
            return Err(AppError::new("download_busy", "下载队列已满。"));
        }
        self.records.lock().await.insert(
            key.clone(),
            Record {
                id: model.id.clone(),
                status: "downloading".into(),
                downloaded: 0,
                total: None,
                error: None,
                path: None,
            },
        );
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let manager = Arc::clone(self);
        let task = tokio::spawn(async move {
            let result = manager.run(&model, &task_cancel).await;
            let key = model.id.to_ascii_lowercase();
            let mut records = manager.records.lock().await;
            if let Some(record) = records.get_mut(&key) {
                match result {
                    Ok(path) => {
                        record.status = "ready".into();
                        record.path = Some(path.to_string_lossy().into_owned());
                        record.error = None;
                    }
                    Err(e) if e.code == "cancelled" => {
                        record.status = "cancelled".into();
                        record.error = None;
                    }
                    Err(e) => {
                        record.status = "error".into();
                        record.error = Some(e.message);
                    }
                }
            }
        });
        control.jobs.insert(key, Job { cancel, task });
        Ok(())
    }

    pub async fn cancel(&self, id: &str) {
        let mut control = self.control.lock().await;
        if let Some(job) = control.jobs.remove(&id.to_ascii_lowercase()) {
            job.cancel.cancel();
            let _ = job.task.await;
        }
    }

    pub async fn snapshot(&self) -> Value {
        json!(self.records.lock().await.values().collect::<Vec<_>>())
    }
    /// Reconstruct paused/complete downloads from owned manifests without network access.
    pub async fn restore(&self, models: &[ModelSpec]) {
        let paths = self.paths.clone();
        let models = models.to_vec();
        let recovered = tokio::task::spawn_blocking(move || {
            let mut records = Vec::new();
            for model in models {
                if !model.path.is_empty() {
                    continue;
                }
                let recover = || -> Result<Option<Record>> {
                    validate_model(&model)?;
                    let owned = Managed::open(&paths.models, &model)?;
                    let Some(manifest) = read_manifest(&owned.manifest)? else {
                        return Ok(None);
                    };
                    if !manifest.belongs_to(&model) {
                        return Err(AppError::new(
                            "not_owned",
                            "模型配置与下载记录不匹配，请恢复原配置或使用新的模型 ID。",
                        ));
                    }
                    let complete = regular_exists(&owned.final_path)?;
                    let partial = regular_exists(&owned.part)?;
                    if !complete && !partial {
                        return Ok(None);
                    }
                    let file = if complete {
                        &owned.final_path
                    } else {
                        &owned.part
                    };
                    let size = std::fs::metadata(file).map_err(|_| AppError::io())?.len();
                    if size > manifest.size || (complete && size != manifest.size) {
                        return Err(AppError::new(
                            "integrity",
                            "模型文件大小与校验记录不符，请删除后重新下载。",
                        ));
                    }
                    Ok(Some(Record {
                        id: model.id.clone(),
                        status: if complete { "ready" } else { "cancelled" }.into(),
                        downloaded: size,
                        total: Some(manifest.size),
                        error: None,
                        path: Some(file.to_string_lossy().into_owned()),
                    }))
                };
                match recover() {
                    Ok(Some(record)) => records.push(record),
                    Ok(None) => {}
                    Err(e) => records.push(Record {
                        id: model.id,
                        status: "error".into(),
                        downloaded: 0,
                        total: None,
                        error: Some(e.message),
                        path: None,
                    }),
                }
            }
            records
        })
        .await;
        if let Ok(recovered) = recovered {
            let mut records = self.records.lock().await;
            for record in recovered {
                records
                    .entry(record.id.to_ascii_lowercase())
                    .or_insert(record);
            }
        }
    }

    pub async fn delete(&self, model: &ModelSpec) -> Result<()> {
        validate_model(model)?; // Imported absolute paths are never deletion targets.
        let mut control = self.control.lock().await;
        if let Some(job) = control.jobs.remove(&model.id.to_ascii_lowercase()) {
            job.cancel.cancel();
            let _ = job.task.await;
        }
        let owned = Managed::open(&self.paths.models, model)?;
        let manifest = read_manifest(&owned.manifest)?;
        let any_data = regular_exists(&owned.final_path)? || regular_exists(&owned.part)?;
        if any_data && !manifest.as_ref().is_some_and(|m| m.belongs_to(model)) {
            return Err(AppError::new(
                "not_owned",
                "文件缺少有效的应用所有权记录，不能删除。",
            ));
        }
        if let Some(m) = manifest {
            if !m.belongs_to(model) {
                return Err(AppError::new(
                    "not_owned",
                    "文件所有权记录不匹配，不能删除。",
                ));
            }
            for path in [&owned.final_path, &owned.part, &owned.manifest] {
                remove_regular(path)?;
            }
        }
        self.records.lock().await.insert(
            model.id.to_ascii_lowercase(),
            Record {
                id: model.id.clone(),
                status: "missing".into(),
                downloaded: 0,
                total: None,
                error: None,
                path: None,
            },
        );
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut control = self.control.lock().await;
        control.closed = true;
        for job in control.jobs.values() {
            job.cancel.cancel();
        }
        for (_, job) in std::mem::take(&mut control.jobs) {
            let _ = job.task.await;
        }
    }

    async fn progress(&self, id: &str, status: &str, downloaded: u64, total: u64) {
        if let Some(record) = self.records.lock().await.get_mut(&id.to_ascii_lowercase()) {
            record.status = status.into();
            record.downloaded = downloaded;
            record.total = Some(total);
        }
    }

    async fn run(&self, model: &ModelSpec, cancel: &CancellationToken) -> Result<PathBuf> {
        let _permit = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            permit = self.slots.acquire() => permit.map_err(|_| cancelled())?,
        };
        let client = self
            .client
            .as_ref()
            .map_err(|_| AppError::new("network", "无法初始化安全网络连接。"))?;
        let token = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            token = credentials(&self.paths.hf_candidates) => token?,
        };
        let owned = Managed::open(&self.paths.models, model)?;
        let mut last = AppError::new("network", "下载连接失败，请重试。");
        for attempt in 0..MAX_ATTEMPTS {
            let result = self.attempt(client, &token, model, &owned, cancel).await;
            match result {
                Ok(()) => return Ok(owned.final_path.clone()),
                Err(e) if e.code == "transient" => last = e,
                Err(e) => return Err(e),
            }
            if attempt + 1 < MAX_ATTEMPTS {
                tokio::select! { _ = cancel.cancelled() => return Err(cancelled()), _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => {} }
            }
        }
        Err(last)
    }

    async fn attempt(
        &self,
        client: &reqwest::Client,
        token: &HeaderValue,
        model: &ModelSpec,
        owned: &Managed,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let remote = fetch_metadata(client, token, model, cancel).await?;
        let old = read_manifest(&owned.manifest)?;
        if let Some(old) = &old {
            if !old.belongs_to(model) {
                return Err(AppError::new(
                    "not_owned",
                    "现有下载记录属于其他文件，请选择不同的模型 ID。",
                ));
            }
        }
        if regular_exists(&owned.final_path)? {
            self.progress(&model.id, "verifying", remote.size, remote.size)
                .await;
            verify(&owned.final_path, &remote, cancel).await?;
            save_manifest(&owned.manifest, &remote)?;
            return Ok(());
        }
        let part_exists = regular_exists(&owned.part)?;
        if part_exists && old.is_none() {
            return Err(AppError::new(
                "not_owned",
                "发现没有可信记录的临时文件；请移走该文件后重试。",
            ));
        }
        if part_exists && old.as_ref() != Some(&remote) {
            remove_regular(&owned.part)?;
        }
        save_manifest(&owned.manifest, &remote)?;
        let file = open_regular(&owned.part, true, true)?;
        let mut offset = file.metadata().map_err(|_| AppError::io())?.len();
        if offset > remote.size {
            file.set_len(0).map_err(|_| AppError::io())?;
            offset = 0;
        }
        let mut file = tokio::fs::File::from_std(file);
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| AppError::io())?;
        self.progress(&model.id, "downloading", offset, remote.size)
            .await;
        if offset < remote.size {
            let url = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                model.source_repo, remote.commit, model.filename
            );
            let mut request = client
                .get(url)
                .header(AUTHORIZATION, token.clone())
                .header("Accept-Encoding", "identity");
            if offset > 0 {
                request = request.header(RANGE, format!("bytes={offset}-"));
            }
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err(cancelled()),
                response = request.send() => response.map_err(|_| transient())?,
            };
            check_status(response.status())?;
            if response
                .headers()
                .get(CONTENT_ENCODING)
                .is_some_and(|h| h != "identity")
            {
                return Err(protocol_error());
            }
            let range = response
                .headers()
                .get(CONTENT_RANGE)
                .map(|v| v.to_str().map_err(|_| protocol_error()))
                .transpose()?;
            let content_len = response.content_length();
            let start =
                validate_response(response.status(), range, content_len, offset, remote.size)?;
            // Servers may ignore Range. A full 200 response must replace, never append.
            if start == 0 && offset != 0 {
                file.set_len(0).await.map_err(|_| AppError::io())?;
                file.seek(std::io::SeekFrom::Start(0))
                    .await
                    .map_err(|_| AppError::io())?;
                offset = 0;
            }
            let mut stream = response.bytes_stream();
            let mut last_update = std::time::Instant::now();
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { file.flush().await.map_err(|_| AppError::io())?; return Err(cancelled()); },
                    chunk = stream.next() => chunk,
                };
                let Some(chunk) = chunk else { break };
                let chunk = chunk.map_err(|_| transient())?;
                let next = offset
                    .checked_add(chunk.len() as u64)
                    .filter(|n| *n <= remote.size)
                    .ok_or_else(protocol_error)?;
                file.write_all(&chunk).await.map_err(|_| AppError::io())?;
                offset = next;
                if last_update.elapsed() >= Duration::from_millis(200) {
                    self.progress(&model.id, "downloading", offset, remote.size)
                        .await;
                    last_update = std::time::Instant::now();
                }
            }
            file.flush().await.map_err(|_| AppError::io())?;
            if offset != remote.size {
                return Err(transient());
            }
        }
        file.sync_all().await.map_err(|_| AppError::io())?;
        drop(file);
        self.progress(&model.id, "verifying", remote.size, remote.size)
            .await;
        if let Err(error) = verify(&owned.part, &remote, cancel).await {
            // Bad bytes must not be retried forever as a complete partial file.
            if error.code == "integrity" {
                remove_regular(&owned.part)?;
            }
            return Err(error);
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        if regular_exists(&owned.final_path)? {
            return Err(AppError::new(
                "file_exists",
                "目标文件已存在，请重新验证后重试。",
            ));
        }
        // Same-directory rename publishes a fully flushed, validated file atomically.
        publish(&owned.part, &owned.final_path)?;
        Ok(())
    }
}

fn validate_model(model: &ModelSpec) -> Result<()> {
    if !model.path.is_empty() {
        return Err(AppError::new(
            "imported_file",
            "本地导入文件不能通过下载管理器下载或删除。",
        ));
    }
    if !valid_id(&model.id) || !valid_repo(&model.source_repo) || !valid_filename(&model.filename) {
        return Err(AppError::invalid("模型 ID、仓库或 GGUF 路径无效。"));
    }
    Ok(())
}
fn cancelled() -> AppError {
    AppError::new("cancelled", "下载已取消。")
}
fn transient() -> AppError {
    AppError::new(
        "transient",
        "下载连接中断或服务暂不可用，已保留进度，请重试。",
    )
}
fn protocol_error() -> AppError {
    AppError::new(
        "download_protocol",
        "服务器返回的文件范围或长度不正确，下载已停止。",
    )
}
fn integrity_error() -> AppError {
    AppError::new("integrity", "文件的 SHA-256、大小或 GGUF 文件头校验失败。")
}
fn path_error() -> AppError {
    AppError::new(
        "unsafe_path",
        "模型目录包含链接、重解析点或不安全的文件类型。",
    )
}
fn credential_invalid() -> AppError {
    AppError::new(
        "credential_invalid",
        "Hugging Face 凭据文件格式或令牌无效，请检查本地 YAML 配置。",
    )
}
fn tempfile_credential_path(data: &Path) -> PathBuf {
    data.join(format!(".huggingface-{}.new", uuid::Uuid::new_v4()))
}
fn check_status(status: StatusCode) -> Result<()> {
    match status.as_u16() {
        200 | 206 => Ok(()),
        401 => Err(AppError::new(
            "credential_invalid",
            "Hugging Face 拒绝了令牌，请更新本地凭据。",
        )),
        403 => Err(AppError::new(
            "forbidden",
            "Hugging Face 拒绝访问，请检查令牌权限及模型访问许可。",
        )),
        404 => Err(AppError::new(
            "not_found",
            "Hugging Face 仓库或文件不存在，或当前令牌没有访问权限。",
        )),
        408 | 429 | 500..=599 => Err(transient()),
        _ => Err(protocol_error()),
    }
}

async fn credentials(candidates: &[PathBuf]) -> Result<HeaderValue> {
    for path in candidates {
        let file = match tokio::fs::File::open(path).await {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(credential_invalid()),
        };
        let meta = file.metadata().await.map_err(|_| credential_invalid())?;
        if !meta.is_file() || meta.len() > 64 * 1024 {
            return Err(credential_invalid());
        }
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| credential_invalid())?;
        if bytes.len() > 64 * 1024 {
            return Err(credential_invalid());
        }
        return parse_credentials(&bytes);
    }
    Err(AppError::new(
        "credential_missing",
        "未找到 huggingface.yml；请在应用目录或应用数据目录配置 Hugging Face 令牌。",
    ))
}
fn parse_credentials(bytes: &[u8]) -> Result<HeaderValue> {
    let yaml: serde_yaml::Value =
        serde_yaml::from_slice(bytes).map_err(|_| credential_invalid())?;
    let token = yaml
        .get("token")
        .or_else(|| yaml.get("hf_token"))
        .or_else(|| yaml.get("huggingface").and_then(|v| v.get("token")))
        .or_else(|| yaml.get("hf").and_then(|v| v.get("token")))
        .and_then(serde_yaml::Value::as_str)
        .ok_or_else(credential_invalid)?;
    if !token.starts_with("hf_")
        || !(10..=512).contains(&token.len())
        || !token[3..].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err(credential_invalid());
    }
    let mut header =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| credential_invalid())?;
    header.set_sensitive(true);
    Ok(header)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    id: String,
    repo: String,
    filename: String,
    commit: String,
    sha256: String,
    size: u64,
}
impl Manifest {
    fn valid(&self) -> bool {
        self.version == 1
            && valid_id(&self.id)
            && valid_repo(&self.repo)
            && valid_filename(&self.filename)
            && valid_hex(&self.commit, 40)
            && valid_hex(&self.sha256, 64)
            && (4..=MAX_MODEL).contains(&self.size)
    }
    fn belongs_to(&self, model: &ModelSpec) -> bool {
        self.valid()
            && self.id == model.id
            && self.repo == model.source_repo
            && self.filename == model.filename
    }
}
fn valid_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}
async fn fetch_metadata(
    client: &reqwest::Client,
    token: &HeaderValue,
    model: &ModelSpec,
    cancel: &CancellationToken,
) -> Result<Manifest> {
    // blobs=true is essential: ordinary siblings responses omit LFS hashes.
    let request = client
        .get(format!(
            "https://huggingface.co/api/models/{}/revision/main",
            model.source_repo
        ))
        .query(&[("blobs", "true")])
        .header(AUTHORIZATION, token.clone())
        .timeout(Duration::from_secs(45));
    let fetch = async {
        let response = request.send().await.map_err(|_| transient())?;
        check_status(response.status())?;
        if response.status() != StatusCode::OK
            || response
                .content_length()
                .is_some_and(|n| n > MAX_METADATA as u64)
        {
            return Err(protocol_error());
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| transient())?;
            if bytes.len().saturating_add(chunk.len()) > MAX_METADATA {
                return Err(protocol_error());
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_metadata(&bytes, model)
    };
    tokio::select! { _ = cancel.cancelled() => Err(cancelled()), result = fetch => result }
}
fn parse_metadata(bytes: &[u8], model: &ModelSpec) -> Result<Manifest> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| protocol_error())?;
    let invalid = || {
        AppError::new(
            "metadata",
            "Hugging Face 未提供可信的提交、LFS SHA-256 或文件大小，无法安全下载。",
        )
    };
    let commit = value
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let siblings = value
        .get("siblings")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut matches = siblings
        .iter()
        .filter(|s| s.get("rfilename").and_then(Value::as_str) == Some(model.filename.as_str()));
    let sibling = matches
        .next()
        .ok_or_else(|| AppError::new("not_found", "仓库中没有指定的 GGUF 文件。"))?;
    if matches.next().is_some() {
        return Err(invalid());
    }
    let lfs = sibling.get("lfs").ok_or_else(invalid)?;
    let size = lfs
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    if sibling
        .get("size")
        .and_then(Value::as_u64)
        .is_some_and(|n| n != size)
    {
        return Err(invalid());
    }
    let result = Manifest {
        version: 1,
        id: model.id.clone(),
        repo: model.source_repo.clone(),
        filename: model.filename.clone(),
        commit: commit.to_ascii_lowercase(),
        sha256: lfs
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            .to_ascii_lowercase(),
        size,
    };
    if !result.valid() {
        return Err(invalid());
    }
    Ok(result)
}

/// Return the first response byte's offset. Only a full 200 or exact remaining 206 is accepted.
fn validate_response(
    status: StatusCode,
    range: Option<&str>,
    length: Option<u64>,
    offset: u64,
    size: u64,
) -> Result<u64> {
    if offset >= size {
        return Err(protocol_error());
    }
    match status {
        StatusCode::OK => {
            if range.is_some() || length.is_some_and(|n| n != size) {
                return Err(protocol_error());
            }
            Ok(0)
        }
        StatusCode::PARTIAL_CONTENT => {
            let range = range
                .and_then(|s| s.strip_prefix("bytes "))
                .ok_or_else(protocol_error)?;
            let (bounds, total) = range.split_once('/').ok_or_else(protocol_error)?;
            let (start, end) = bounds.split_once('-').ok_or_else(protocol_error)?;
            let parse = |v: &str| -> Result<u64> {
                if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(protocol_error());
                }
                v.parse().map_err(|_| protocol_error())
            };
            if parse(start)? != offset
                || parse(end)? != size - 1
                || parse(total)? != size
                || length.is_some_and(|n| n != size - offset)
            {
                return Err(protocol_error());
            }
            Ok(offset)
        }
        _ => Err(protocol_error()),
    }
}

async fn verify(path: &Path, expected: &Manifest, cancel: &CancellationToken) -> Result<()> {
    let file = open_regular(path, false, false)?;
    if file.metadata().map_err(|_| AppError::io())?.len() != expected.size {
        return Err(integrity_error());
    }
    let mut file = tokio::fs::File::from_std(file);
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut magic = Vec::with_capacity(4);
    let mut total = 0u64;
    loop {
        let count = tokio::select! { biased; _ = cancel.cancelled() => return Err(cancelled()), n = file.read(&mut buffer) => n.map_err(|_| AppError::io())? };
        if count == 0 {
            break;
        }
        let needed = (4 - magic.len()).min(count);
        magic.extend_from_slice(&buffer[..needed]);
        total += count as u64;
        if total > expected.size {
            return Err(integrity_error());
        }
        hash.update(&buffer[..count]);
    }
    if total != expected.size || magic != b"GGUF" || hex::encode(hash.finalize()) != expected.sha256
    {
        return Err(integrity_error());
    }
    Ok(())
}

struct Managed {
    final_path: PathBuf,
    part: PathBuf,
    manifest: PathBuf,
    // Windows handles deny FILE_SHARE_DELETE for every ancestor, preventing junction swaps.
    _directories: Vec<File>,
}
impl Managed {
    fn open(root: &Path, model: &ModelSpec) -> Result<Self> {
        validate_model(model)?;
        let absolute = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|_| AppError::io())?
                .join(root)
        };
        let mut directories = Vec::new();
        let mut current = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::ParentDir | Component::CurDir => return Err(path_error()),
                Component::Prefix(_) | Component::RootDir => {
                    current.push(component.as_os_str());
                    continue;
                }
                Component::Normal(_) => current.push(component.as_os_str()),
            }
            ensure_directory(&current, &mut directories)?;
        }
        current.push(&model.id);
        ensure_directory(&current, &mut directories)?;
        let leaf = model.filename.rsplit('/').next().ok_or_else(path_error)?;
        let final_path = current.join(leaf);
        let part = current.join(format!("{leaf}.part"));
        let manifest = current.join(format!("{leaf}.download.json"));
        for path in [&final_path, &part, &manifest] {
            regular_exists(path)?;
        }
        Ok(Self {
            final_path,
            part,
            manifest,
            _directories: directories,
        })
    }
}
fn suspicious(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        } // FILE_ATTRIBUTE_REPARSE_POINT
    }
    false
}
fn ensure_directory(path: &Path, handles: &mut Vec<File>) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if suspicious(&meta) || !meta.is_dir() => return Err(path_error()),
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|_| AppError::io())?
        }
        Err(_) => return Err(AppError::io()),
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .access_mode(0)
            .share_mode(3)
            .custom_flags(0x02000000 | 0x00200000)
            .open(path)
            .map_err(|_| path_error())?;
        let meta = file.metadata().map_err(|_| path_error())?;
        if suspicious(&meta) || !meta.is_dir() {
            return Err(path_error());
        }
        handles.push(file);
    }
    #[cfg(not(windows))]
    {
        let _ = handles;
    }
    Ok(())
}
fn regular_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if suspicious(&meta) || !meta.is_file() => Err(path_error()),
        Ok(meta) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err(path_error());
                }
            }
            let _ = meta;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(AppError::io()),
    }
}
fn open_regular(path: &Path, write: bool, create: bool) -> Result<File> {
    regular_exists(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(write).create(create);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000); // OPEN_REPARSE_POINT, no writers/deleters
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x20000);
    } // O_NOFOLLOW
    let file = options.open(path).map_err(|_| AppError::io())?;
    let meta = file.metadata().map_err(|_| AppError::io())?;
    if suspicious(&meta) || !meta.is_file() {
        return Err(path_error());
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // Reject hard links too: truncating a partial must never mutate another path.
        let mut info = std::mem::MaybeUninit::<FileInformation>::zeroed();
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) };
        if ok == 0 || unsafe { info.assume_init() }.number_of_links != 1 {
            return Err(path_error());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(path_error());
        }
    }
    Ok(file)
}
// This small stable Win32 ABI avoids adding a dependency solely for handle link counts.
#[cfg(windows)]
#[repr(C)]
struct FileInformation {
    attributes: u32,
    creation: [u32; 2],
    access: [u32; 2],
    write: [u32; 2],
    volume_serial: u32,
    size_high: u32,
    size_low: u32,
    number_of_links: u32,
    index_high: u32,
    index_low: u32,
}
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut FileInformation)
        -> i32;
    fn MoveFileExW(existing: *const u16, destination: *const u16, flags: u32) -> i32;
}
fn publish(part: &Path, destination: &Path) -> Result<()> {
    regular_exists(part)?;
    if regular_exists(destination)? {
        return Err(AppError::new(
            "file_exists",
            "目标文件已存在，未覆盖现有文件。",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let source: Vec<u16> = part.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // MOVEFILE_WRITE_THROUGH without REPLACE_EXISTING: a racing file is never overwritten.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 8) } == 0 {
            return Err(AppError::io());
        }
    }
    #[cfg(not(windows))]
    {
        // Link publication is atomic and cannot replace an existing name.
        std::fs::hard_link(part, destination).map_err(|_| AppError::io())?;
        std::fs::remove_file(part).map_err(|_| AppError::io())?;
    }
    Ok(())
}
fn remove_regular(path: &Path) -> Result<()> {
    if regular_exists(path)? {
        // Validate the opened object, including link count, before deleting its managed name.
        drop(open_regular(path, false, false)?);
        std::fs::remove_file(path).map_err(|_| AppError::io())?;
    }
    Ok(())
}
fn read_manifest(path: &Path) -> Result<Option<Manifest>> {
    if !regular_exists(path)? {
        return Ok(None);
    }
    let file = open_regular(path, false, false)?;
    if file.metadata().map_err(|_| AppError::io())?.len() > 4096 {
        return Err(path_error());
    }
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| AppError::io())?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::new("metadata", "本地下载记录损坏，无法安全恢复或删除。"))?;
    if !manifest.valid() {
        return Err(AppError::new(
            "metadata",
            "本地下载记录无效，无法安全恢复或删除。",
        ));
    }
    Ok(Some(manifest))
}
fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    if read_manifest(path)?.as_ref() == Some(manifest) {
        return Ok(());
    }
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(manifest).map_err(|_| AppError::io())?;
    let result = (|| {
        use std::io::Write;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| AppError::io())?;
        file.write_all(&bytes).map_err(|_| AppError::io())?;
        file.sync_all().map_err(|_| AppError::io())?;
        drop(file);
        regular_exists(path)?;
        std::fs::rename(&temp, path).map_err(|_| AppError::io())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    fn model() -> ModelSpec {
        Settings::default().models.remove(0)
    }
    fn manifest(bytes: &[u8]) -> Manifest {
        let m = model();
        Manifest {
            version: 1,
            id: m.id,
            repo: m.source_repo,
            filename: m.filename,
            commit: "a".repeat(40),
            sha256: hex::encode(Sha256::digest(bytes)),
            size: bytes.len() as u64,
        }
    }
    #[test]
    fn validates_windows_traversal_and_import_boundary() {
        let mut m = model();
        for id in ["../x", "C:", "CON", "NUL", "LPT1", "a/b", "a\\b", "", "x."] {
            m.id = id.into();
            assert!(validate_model(&m).is_err());
        }
        m = model();
        for name in [
            "../x.gguf",
            "/x.gguf",
            "C:/x.gguf",
            "a\\x.gguf",
            "a//x.gguf",
            "a/./x.gguf",
            "a/../x.gguf",
            "CON.gguf",
            "x.gguf:stream",
            "x .gguf",
            "a./x.gguf",
        ] {
            m.filename = name.into();
            assert!(validate_model(&m).is_err(), "{name}");
        }
        m = model();
        m.path = "C:/user/important.gguf".into();
        assert_eq!(validate_model(&m).unwrap_err().code, "imported_file");
        m = model();
        m.filename = "weights/model.gguf".into();
        assert!(validate_model(&m).is_ok());
    }
    #[tokio::test]
    async fn restored_partial_download_shows_resume_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().into(), dir.path().into());
        let model = model();
        let owned = Managed::open(&paths.models, &model).unwrap();
        save_manifest(&owned.manifest, &manifest(b"GGUFtest")).unwrap();
        std::fs::write(&owned.part, b"GGUF").unwrap();
        let manager = DownloadManager::new(paths);
        manager.restore(&[model]).await;
        let state = manager.snapshot().await;
        assert_eq!(state[0]["status"], "cancelled");
        assert_eq!(state[0]["downloaded"], 4);
        assert_eq!(state[0]["total"], 8);
        manager.shutdown().await;
    }
    #[test]
    fn yaml_shapes_and_errors_do_not_disclose_secrets() {
        for source in [
            "token: hf_0123456789ABC",
            "hf_token: hf_0123456789ABC",
            "huggingface:\n  token: hf_0123456789ABC",
            "hf:\n  token: hf_0123456789ABC",
        ] {
            let value = parse_credentials(source.as_bytes()).unwrap();
            assert!(value.is_sensitive());
        }
        for source in [
            "token: [hf_PRIVATE_SECRET",
            "token: hf_PRIVATE_SECRET!",
            "hf_token: 42",
            "huggingface: []",
            "token: null",
            "token: 'hf_abc\r\nAuthorization: private'",
            "[]",
        ] {
            let err = parse_credentials(source.as_bytes()).unwrap_err();
            let visible = format!("{err:?} {}", serde_json::to_string(&err).unwrap());
            assert!(!visible.contains("PRIVATE"));
            assert!(!visible.contains("Authorization"));
            assert_eq!(err.code, "credential_invalid");
        }
        assert_eq!(
            check_status(StatusCode::UNAUTHORIZED).unwrap_err().code,
            "credential_invalid"
        );
        assert_eq!(
            check_status(StatusCode::FORBIDDEN).unwrap_err().code,
            "forbidden"
        );
        assert_eq!(
            check_status(StatusCode::NOT_FOUND).unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            check_status(StatusCode::TOO_MANY_REQUESTS)
                .unwrap_err()
                .code,
            "transient"
        );
    }
    #[tokio::test]
    async fn saved_credentials_override_legacy_files_and_never_echo() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy.yml");
        tokio::fs::write(&legacy, b"broken YAML: [").await.unwrap();
        let mut paths = Paths::new(dir.path().join("data"), dir.path().into());
        paths.prepare().await.unwrap();
        paths.hf_candidates = vec![paths.data.join("huggingface.yml"), legacy];
        let manager = DownloadManager::new(paths.clone());
        let first = "hf_FirstTestCredential123";
        manager.save_credential(first).await.unwrap();
        assert_eq!(
            credentials(&paths.hf_candidates)
                .await
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {first}")
        );
        let second = "hf_ReplacementTestCredential456";
        manager.save_credential(second).await.unwrap();
        assert_eq!(
            credentials(&paths.hf_candidates)
                .await
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {second}")
        );
        let error = manager
            .save_credential("hf_PRIVATE_INVALID!")
            .await
            .unwrap_err();
        assert!(!serde_json::to_string(&error).unwrap().contains("PRIVATE"));
        assert_eq!(
            credentials(&paths.hf_candidates)
                .await
                .unwrap()
                .to_str()
                .unwrap(),
            format!("Bearer {second}")
        );
        let status = manager.credential_status().await;
        assert_eq!(status["status"], "ready");
        assert!(!status.to_string().contains(first));
        assert!(!status.to_string().contains(second));
        let mut entries = tokio::fs::read_dir(&paths.data).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".new"));
        }
    }
    #[test]
    fn exact_ranges_and_ignored_ranges() {
        assert_eq!(
            validate_response(
                StatusCode::PARTIAL_CONTENT,
                Some("bytes 5-9/10"),
                Some(5),
                5,
                10
            )
            .unwrap(),
            5
        );
        assert_eq!(
            validate_response(StatusCode::OK, None, Some(10), 5, 10).unwrap(),
            0
        );
        assert_eq!(
            validate_response(StatusCode::OK, None, None, 0, 10).unwrap(),
            0
        );
        for range in [
            "bytes 0-9/10",
            "bytes 5-8/10",
            "bytes 5-10/10",
            "bytes 5-9/*",
            "bytes 5-9/11",
            "bytes +5-9/10",
            "bytes 5-9/10 extra",
            "items 5-9/10",
            "bytes */10",
        ] {
            assert!(
                validate_response(StatusCode::PARTIAL_CONTENT, Some(range), Some(5), 5, 10)
                    .is_err(),
                "{range}"
            );
        }
        assert!(validate_response(
            StatusCode::PARTIAL_CONTENT,
            Some("bytes 5-9/10"),
            Some(10),
            5,
            10
        )
        .is_err());
        assert!(validate_response(StatusCode::OK, None, Some(5), 5, 10).is_err());
        assert!(validate_response(StatusCode::RANGE_NOT_SATISFIABLE, None, None, 10, 10).is_err());
    }
    #[test]
    fn requires_commit_lfs_hash_and_consistent_size() {
        let m = model();
        let expected = manifest(b"GGUFtest");
        let mut response = json!({"sha":expected.commit,"siblings":[{"rfilename":m.filename,"size":8,"lfs":{"size":8,"sha256":expected.sha256}}]});
        assert_eq!(
            parse_metadata(&serde_json::to_vec(&response).unwrap(), &m).unwrap(),
            expected
        );
        response["siblings"][0]["size"] = json!(9);
        assert!(parse_metadata(&serde_json::to_vec(&response).unwrap(), &m).is_err());
        response["siblings"][0]["size"] = json!(8);
        response["sha"] = json!("main");
        assert!(parse_metadata(&serde_json::to_vec(&response).unwrap(), &m).is_err());
        response["sha"] = json!("b".repeat(40));
        response["siblings"][0]["lfs"] = Value::Null;
        assert!(parse_metadata(&serde_json::to_vec(&response).unwrap(), &m).is_err());
    }
    #[tokio::test]
    async fn verifies_digest_size_magic_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.gguf");
        let bytes = b"GGUFtest";
        let expected = manifest(bytes);
        let cancel = CancellationToken::new();
        std::fs::write(&path, bytes).unwrap();
        verify(&path, &expected, &cancel).await.unwrap();
        std::fs::write(&path, b"GGUFbad!").unwrap();
        assert_eq!(
            verify(&path, &expected, &cancel).await.unwrap_err().code,
            "integrity"
        );
        std::fs::write(&path, b"GGUF").unwrap();
        assert!(verify(&path, &expected, &cancel).await.is_err());
        std::fs::write(&path, b"HTMLtest").unwrap();
        assert!(verify(&path, &manifest(b"HTMLtest"), &cancel)
            .await
            .is_err());
        std::fs::write(&path, bytes).unwrap();
        cancel.cancel();
        assert_eq!(
            verify(&path, &expected, &cancel).await.unwrap_err().code,
            "cancelled"
        );
    }
    #[tokio::test]
    async fn ownership_survives_restart_and_delete_refuses_imports_and_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().into(), dir.path().into());
        let m = model();
        let owned = Managed::open(&paths.models, &m).unwrap();
        std::fs::write(&owned.part, b"GGUF").unwrap();
        let manager = DownloadManager::new(paths.clone());
        assert_eq!(manager.delete(&m).await.unwrap_err().code, "not_owned");
        let expected = manifest(b"GGUFtest");
        save_manifest(&owned.manifest, &expected).unwrap();
        assert_eq!(read_manifest(&owned.manifest).unwrap(), Some(expected));
        let final_path = owned.final_path.clone();
        let part = owned.part.clone();
        drop(owned);
        let restarted = DownloadManager::new(paths);
        restarted.delete(&m).await.unwrap();
        assert!(!part.exists());
        let import = dir.path().join("import.gguf");
        std::fs::write(&import, b"GGUFtest").unwrap();
        let mut local = m;
        local.path = import.to_string_lossy().into_owned();
        assert!(restarted.delete(&local).await.is_err());
        assert!(import.exists());
        assert!(!final_path.exists());
        assert_eq!(restarted.snapshot().await[0]["status"], "missing");
    }
    #[test]
    fn rejects_hardlinked_part_before_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let link = dir.path().join("part");
        std::fs::write(&source, b"protected").unwrap();
        std::fs::hard_link(&source, &link).unwrap();
        assert!(open_regular(&link, true, true).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"protected");
    }
    #[cfg(unix)]
    #[test]
    fn rejects_symlink_ancestors_and_files() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        symlink(outside.path(), &root).unwrap();
        assert!(Managed::open(&root, &model()).is_err());
        std::fs::remove_file(&root).unwrap();
        let owned = Managed::open(&root, &model()).unwrap();
        let target = outside.path().join("important");
        std::fs::write(&target, b"safe").unwrap();
        symlink(&target, &owned.part).unwrap();
        assert!(Managed::open(&root, &model()).is_err());
        assert!(remove_regular(&owned.part).is_err());
        assert_eq!(std::fs::read(target).unwrap(), b"safe");
    }
    #[tokio::test]
    async fn credential_lookup_missing_invalid_and_priority() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.yml");
        let second = dir.path().join("second.yml");
        assert_eq!(
            credentials(&[first.clone()]).await.unwrap_err().code,
            "credential_missing"
        );
        std::fs::write(&second, "token: hf_0123456789ABC").unwrap();
        assert!(credentials(&[first.clone(), second.clone()]).await.is_ok());
        std::fs::write(&first, "token: [hf_PRIVATE_SECRET").unwrap();
        assert_eq!(
            credentials(&[first, second]).await.unwrap_err().code,
            "credential_invalid"
        );
    }
    #[test]
    fn atomic_publication_does_not_replace_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("part");
        let destination = dir.path().join("final.gguf");
        std::fs::write(&part, b"GGUFnew").unwrap();
        std::fs::write(&destination, b"GGUFexisting").unwrap();
        assert!(publish(&part, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"GGUFexisting");
        std::fs::remove_file(&destination).unwrap();
        publish(&part, &destination).unwrap();
        assert!(!part.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"GGUFnew");
    }
    #[cfg(windows)]
    #[test]
    fn rejects_windows_junction_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().join("models");
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&root)
            .arg(outside.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "Could not create junction test fixture");
        assert!(Managed::open(&root, &model()).is_err());
        std::fs::remove_dir(&root).unwrap();
        assert!(outside.path().exists());
    }
    #[tokio::test]
    async fn shutdown_prevents_new_work() {
        let dir = tempfile::tempdir().unwrap();
        let manager = DownloadManager::new(Paths::new(dir.path().into(), dir.path().into()));
        manager.shutdown().await;
        assert_eq!(manager.start(model()).await.unwrap_err().code, "shutdown");
    }
}
