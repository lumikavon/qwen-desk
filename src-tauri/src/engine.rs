//! Native llama.cpp process isolation, bounded inference and cancellation.
use crate::{
    config::{InferenceSettings, ModelKind, ModelSpec, ServiceSettings},
    error::{AppError, Result},
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

const MAX_RESPONSE: usize = 32 * 1024 * 1024;
const MAX_INPUT: usize = 512 * 1024;
fn lock<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn cancelled() -> AppError {
    AppError::new("cancelled", "推理请求已取消。")
}
fn timed_out() -> AppError {
    AppError::new("timeout", "推理超时，请减少输入或增加超时时间。")
}
fn unavailable() -> AppError {
    AppError::new("engine_unavailable", "推理引擎不可用，请重新加载模型。")
}
fn invalid_response() -> AppError {
    AppError::new("invalid_response", "推理引擎返回了无效或不完整的响应。")
}

struct Connection {
    url: String,
    key: String,
    model: ModelSpec,
    params: InferenceSettings,
}
struct SlotState {
    status: &'static str,
    model: Option<ModelSpec>,
    mode: String,
    error: Option<AppError>,
    connection: Option<Arc<Connection>>,
    epoch: CancellationToken,
}
impl Default for SlotState {
    fn default() -> Self {
        Self {
            status: "unloaded",
            model: None,
            mode: String::new(),
            error: None,
            connection: None,
            epoch: CancellationToken::new(),
        }
    }
}
struct Slot {
    process: Mutex<Option<NativeProcess>>,
    state: StdMutex<SlotState>,
}
impl Slot {
    fn new() -> Self {
        Self {
            process: Mutex::new(None),
            state: StdMutex::new(SlotState::default()),
        }
    }
}
struct RequestRecord {
    cancel: CancellationToken,
    kind: ModelKind,
}
pub struct EngineManager {
    runtime: PathBuf,
    client: reqwest::Client,
    generation: Slot,
    embedding: Slot,
    requests: StdMutex<HashMap<String, RequestRecord>>,
    last: StdMutex<Value>,
    closed: AtomicBool,
}
pub struct Inference {
    pub stream: Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>,
    pub content_type: String,
}

impl EngineManager {
    pub fn new(runtime: PathBuf) -> Arc<Self> {
        // Loopback traffic must never traverse an HTTP proxy or follow a redirect.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("local HTTP client initialization");
        Arc::new(Self {
            runtime,
            client,
            generation: Slot::new(),
            embedding: Slot::new(),
            requests: StdMutex::new(HashMap::new()),
            last: StdMutex::new(Value::Null),
            closed: AtomicBool::new(false),
        })
    }
    fn slot(&self, kind: ModelKind) -> &Slot {
        match kind {
            ModelKind::Generation => &self.generation,
            ModelKind::Embedding => &self.embedding,
        }
    }
    pub async fn load(&self, model: ModelSpec, params: InferenceSettings) -> Result<()> {
        model.validate()?;
        params.validate()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(AppError::new("shutdown", "应用正在退出。"));
        }
        validate_file(Path::new(&model.path)).await?;
        let slot = self.slot(model.kind);
        let mut process = slot
            .process
            .try_lock()
            .map_err(|_| AppError::new("model_busy", "模型正在切换，请等待或取消加载。"))?;
        let epoch = CancellationToken::new();
        {
            let mut state = lock(&slot.state);
            state.epoch.cancel();
            *state = SlotState {
                status: "loading",
                model: Some(model.clone()),
                mode: String::new(),
                error: None,
                connection: None,
                epoch: epoch.clone(),
            };
        }
        if let Some(mut old) = process.take() {
            old.stop().await;
        }
        if self.closed.load(Ordering::Acquire) {
            epoch.cancel();
        }
        let device = if params.gpu_layers != 0 {
            find_gpu(&self.runtime.join("vulkan/llama-server.exe"), &epoch).await
        } else {
            None
        };
        let mut fallback = false;
        let mut result = if let Some(device) = device {
            let attempt = self.launch(&model, &params, Some(&device), &epoch).await;
            if attempt.is_err() && !epoch.is_cancelled() {
                fallback = true;
                self.launch(&model, &params, None, &epoch).await
            } else {
                attempt
            }
        } else {
            fallback = params.gpu_layers != 0;
            self.launch(&model, &params, None, &epoch).await
        };
        if epoch.is_cancelled() {
            if let Ok((native, _, _)) = &mut result {
                native.stop().await;
            }
            result = Err(cancelled());
        }
        match result {
            Ok((native, connection, mode)) => {
                *process = Some(native);
                let mut state = lock(&slot.state);
                state.status = "loaded";
                state.mode = mode;
                state.connection = Some(Arc::new(connection));
                state.error = if fallback {
                    Some(AppError::new(
                        "cpu_fallback",
                        "GPU 不可用或加载失败，已自动使用 CPU。可减小上下文或更新显卡驱动后重试。",
                    ))
                } else {
                    None
                };
                tracing::info!(event="model_loaded",kind=%model.kind,mode=%state.mode);
                Ok(())
            }
            Err(error) => {
                let mut state = lock(&slot.state);
                state.status = if epoch.is_cancelled() {
                    "unloaded"
                } else {
                    "error"
                };
                state.error = Some(error.clone());
                state.connection = None;
                Err(error)
            }
        }
    }
    async fn launch(
        &self,
        model: &ModelSpec,
        params: &InferenceSettings,
        device: Option<&GpuDevice>,
        cancel: &CancellationToken,
    ) -> Result<(NativeProcess, Connection, String)> {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let folder = self
            .runtime
            .join(if device.is_some() { "vulkan" } else { "cpu" });
        let executable = folder.join("llama-server.exe");
        if !tokio::fs::try_exists(&executable).await.unwrap_or(false) {
            return Err(AppError::new(
                "runtime_missing",
                "原生推理运行库缺失，请修复安装或重新运行运行库准备脚本。",
            ));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| unavailable())?;
        let port = listener.local_addr().map_err(|_| unavailable())?.port();
        let key = uuid::Uuid::new_v4().simple().to_string();
        let mut command = clean_command(&executable);
        command
            .current_dir(&folder)
            .env("LLAMA_API_KEY", &key)
            .args([
                "--model",
                &model.path,
                "--alias",
                &model.name,
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--ctx-size",
                &params.context_size.to_string(),
                "--batch-size",
                &params.batch_size.to_string(),
                "--ubatch-size",
                &params.batch_size.to_string(),
                "--threads",
                &params.threads.to_string(),
                "--parallel",
                "1",
                "--cache-type-k",
                &params.cache_type,
                "--cache-type-v",
                &params.cache_type,
                "--flash-attn",
                "on",
                "--no-webui",
                "--no-slots",
                "--no-agent",
                "--no-ui-mcp-proxy",
                "--timeout",
                "1800",
                "--check-tensors",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(device) = device {
            let layers = if params.gpu_layers < 0 {
                "auto".to_owned()
            } else {
                params.gpu_layers.to_string()
            };
            command.args([
                "--device",
                &device.id,
                "--gpu-layers",
                &layers,
                "--fit",
                "on",
                "--fit-target",
                &params.gpu_memory_mb.to_string(),
            ]);
        } else {
            command.args(["--gpu-layers", "0", "--device", "none"]);
        }
        if model.kind == ModelKind::Embedding {
            command.args(["--embedding", "--embd-normalize", "2"]);
        }
        drop(listener);
        let child = command.spawn().map_err(|_| {
            AppError::new(
                "runtime_launch_failed",
                "无法启动原生推理引擎，请检查运行库及系统依赖。",
            )
        })?;
        let mut native = NativeProcess::new(child)?;
        let connection = Connection {
            url: format!("http://127.0.0.1:{port}"),
            key,
            model: model.clone(),
            params: params.clone(),
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        loop {
            if native
                .child
                .try_wait()
                .map_err(|_| unavailable())?
                .is_some()
            {
                let error = if native.oom.load(Ordering::Relaxed) {
                    AppError::new(
                        "out_of_memory",
                        "模型加载所需内存或显存不足，请减小上下文、批量或 GPU 层数。",
                    )
                } else {
                    AppError::new(
                        "model_load_failed",
                        "模型加载失败，请检查 GGUF 是否完整、架构是否兼容或减小推理参数。",
                    )
                };
                native.stop().await;
                return Err(error);
            }
            let health = self
                .client
                .get(format!("{}/health", connection.url))
                .bearer_auth(&connection.key)
                .timeout(Duration::from_secs(2))
                .send();
            tokio::select! {
                biased;
                _=cancel.cancelled()=>{native.stop().await;return Err(cancelled());},
                _=tokio::time::sleep_until(deadline)=>{native.stop().await;return Err(AppError::new("load_timeout","模型加载超过 180 秒，请减小参数后重试。"));},
                response=health=>{if response.is_ok_and(|r|r.status().is_success()){
                    let mode=device.map(|d|format!("Vulkan / {}",d.name)).unwrap_or_else(||"CPU".into());
                    return Ok((native,connection,mode));
                }}
            }
            tokio::select! {_=cancel.cancelled()=>{},_=tokio::time::sleep(Duration::from_millis(150))=>{}}
        }
    }
    pub async fn unload(&self, kind: ModelKind) -> Result<()> {
        let slot = self.slot(kind);
        {
            let mut state = lock(&slot.state);
            state.epoch.cancel();
            state.connection = None;
            if state.status != "unloaded" {
                state.status = "unloading";
            }
        }
        let mut process = slot.process.lock().await;
        if let Some(mut native) = process.take() {
            native.stop().await;
        }
        *lock(&slot.state) = SlotState::default();
        tracing::info!(event="model_unloaded",kind=%kind);
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        for request in lock(&self.requests).values() {
            request.cancel.cancel();
        }
        let _ = tokio::join!(
            self.unload(ModelKind::Generation),
            self.unload(ModelKind::Embedding)
        );
    }
    pub async fn cancel(&self, id: &str) {
        if let Some(request) = lock(&self.requests).get(id) {
            request.cancel.cancel();
        }
    }
    pub async fn snapshot(&self) -> Value {
        let mut snapshot = json!({});
        for kind in [ModelKind::Generation, ModelKind::Embedding] {
            let slot = self.slot(kind);
            if let Ok(mut process) = slot.process.try_lock() {
                if process
                    .as_mut()
                    .is_some_and(|p| p.child.try_wait().ok().flatten().is_some())
                {
                    process.take();
                    let mut state = lock(&slot.state);
                    state.status = "error";
                    state.error = Some(unavailable());
                    state.connection = None;
                    state.epoch.cancel();
                }
            }
            let state = lock(&slot.state);
            let count = lock(&self.requests)
                .values()
                .filter(|r| r.kind == kind)
                .count();
            snapshot[kind.to_string()] = json!({"status":state.status,"model":state.model,"mode":state.mode,"error":state.error,"active_requests":count});
        }
        snapshot["last_inference"] = lock(&self.last).clone();
        snapshot
    }
    pub async fn models(&self) -> Value {
        let mut data = vec![];
        for kind in [ModelKind::Generation, ModelKind::Embedding] {
            let state = lock(&self.slot(kind).state);
            if let Some(c) = &state.connection {
                data.push(
                    json!({"id":c.model.name,"object":"model","created":0,"owned_by":"local"}),
                );
            }
        }
        json!({"object":"list","data":data})
    }
    pub async fn infer(
        self: &Arc<Self>,
        kind: ModelKind,
        body: Value,
        request_id: String,
        limits: ServiceSettings,
    ) -> Result<Inference> {
        limits.validate()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(AppError::invalid("请求 ID 无效。"));
        }
        let (connection, epoch) = {
            let state = lock(&self.slot(kind).state);
            (
                state
                    .connection
                    .clone()
                    .ok_or_else(|| AppError::new("model_not_loaded", "请先加载对应的本地模型。"))?,
                state.epoch.clone(),
            )
        };
        let validated = validate_request(
            kind,
            body,
            &connection.model.name,
            connection.params.context_size,
        )?;
        let cancel = CancellationToken::new();
        {
            let mut requests = lock(&self.requests);
            if requests.contains_key(&request_id) {
                return Err(AppError::new("duplicate_request", "请求 ID 已在执行。"));
            }
            if requests.len() >= limits.concurrency {
                return Err(AppError::new(
                    "rate_limit_exceeded",
                    "推理并发已达到上限，请稍后重试。",
                ));
            }
            requests.insert(
                request_id.clone(),
                RequestRecord {
                    cancel: cancel.clone(),
                    kind,
                },
            );
        }
        let mut guard = RequestGuard {
            manager: self.clone(),
            id: request_id,
            kind,
            start: Instant::now(),
            finished: false,
            usage: Value::Null,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(limits.timeout_secs);
        let route = if kind == ModelKind::Generation {
            "chat/completions"
        } else {
            "embeddings"
        };
        let request = self
            .client
            .post(format!("{}/v1/{route}", connection.url))
            .bearer_auth(&connection.key)
            .json(&validated.body)
            .send();
        let response = tokio::select! {biased;_=cancel.cancelled()=>Err(cancelled()),_=epoch.cancelled()=>Err(cancelled()),_=tokio::time::sleep_until(deadline)=>Err(timed_out()),response=request=>response.map_err(|_|unavailable())};
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                guard.finish(&e.code);
                return Err(e);
            }
        };
        if !response.status().is_success() {
            let error = match response.status().as_u16() {
                400 | 413 | 422 => AppError::new(
                    "context_limit",
                    "请求超过模型上下文或不被此模型支持，请减少文本和输出长度。",
                ),
                429 | 503 => AppError::new("engine_busy", "推理引擎繁忙，请稍后重试。"),
                _ => unavailable(),
            };
            guard.finish(&error.code);
            return Err(error);
        }
        let streaming = validated.streaming;
        let stream = async_stream::stream! {
            let mut upstream=response.bytes_stream();let mut buffer=Vec::new();let mut total=0usize;let mut done=false;
            loop {
                let next=tokio::select!{biased;_=cancel.cancelled()=>Some(Err(cancelled())),_=epoch.cancelled()=>Some(Err(cancelled())),_=tokio::time::sleep_until(deadline)=>Some(Err(timed_out())),next=upstream.next()=>next.map(|v|v.map_err(|_|unavailable()))};
                match next {
                    Some(Err(e))=>{guard.finish(&e.code);yield Err(e);break;},
                    Some(Ok(chunk))=>{
                        total=total.saturating_add(chunk.len());if total>MAX_RESPONSE{let e=AppError::new("response_too_large","推理响应超过限制。");guard.finish(&e.code);yield Err(e);break;}
                        buffer.extend_from_slice(&chunk);
                        if streaming{
                            while let Some((end,delimiter))=frame_boundary(&buffer){
                                if cancel.is_cancelled() || epoch.is_cancelled() { guard.finish("cancelled"); yield Err(cancelled()); return; }
                                if tokio::time::Instant::now() >= deadline { guard.finish("timeout"); yield Err(timed_out()); return; }
                                let frame=buffer[..end].to_vec();buffer.drain(..end+delimiter);
                                let text=match std::str::from_utf8(&frame){Ok(t)=>t,Err(_)=>{guard.finish("invalid_response");yield Err(invalid_response());return;}};
                                let data=text.lines().filter_map(|line|line.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
                                if data.is_empty(){continue;}
                                if data.trim()=="[DONE]"{done=true;guard.finish("completed");yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));break;}
                                let value=match serde_json::from_str::<Value>(&data){Ok(v)=>v,Err(_)=>{guard.finish("invalid_response");yield Err(invalid_response());return;}};
                                if value.get("error").is_some(){guard.finish("engine_error");yield Err(unavailable());return;}
                                if let Some(usage)=value.get("usage").filter(|v|!v.is_null()){guard.usage=usage.clone();}
                                let clean=sanitize_chat(value,true,&connection.model.name);
                                yield Ok(Bytes::from(format!("data: {}\n\n",clean)));
                            }
                            if done{break;}
                            if buffer.len()>1024*1024{guard.finish("invalid_response");yield Err(invalid_response());break;}
                        }
                    },
                    None=>{
                        if streaming{if !done{guard.finish("invalid_response");yield Err(invalid_response());}}
                        else{
                            let parsed=serde_json::from_slice::<Value>(&buffer).map_err(|_|invalid_response());
                            match parsed.and_then(|v|if kind==ModelKind::Embedding{sanitize_embeddings(v,&connection.model.name,validated.dimensions,validated.base64)}else if v.get("error").is_some()||!v["choices"].is_array(){Err(invalid_response())}else{Ok(sanitize_chat(v,false,&connection.model.name))}){
                                Ok(value)=>{guard.usage=value["usage"].clone();guard.finish("completed");yield Ok(Bytes::from(value.to_string()));},
                                Err(e)=>{guard.finish(&e.code);yield Err(e);}
                            }
                        }
                        break;
                    }
                }
            }
        };
        Ok(Inference {
            stream: Box::pin(stream),
            content_type: if streaming {
                "text/event-stream"
            } else {
                "application/json"
            }
            .into(),
        })
    }
}

struct RequestGuard {
    manager: Arc<EngineManager>,
    id: String,
    kind: ModelKind,
    start: Instant,
    finished: bool,
    usage: Value,
}
impl RequestGuard {
    fn finish(&mut self, status: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        lock(&self.manager.requests).remove(&self.id);
        let elapsed = self.start.elapsed().as_millis() as u64;
        let completion = self.usage["completion_tokens"].as_u64().unwrap_or(0);
        *lock(&self.manager.last) = json!({"request_id":self.id,"kind":self.kind,"status":status,"elapsed_ms":elapsed,"duration_ms":elapsed,"usage":self.usage,"prompt_tokens":self.usage["prompt_tokens"],"completion_tokens":self.usage["completion_tokens"],"tokens_per_second":if elapsed>0{completion as f64*1000.0/elapsed as f64}else{0.0}});
        tracing::info!(event="inference_finished",kind=%self.kind,status,elapsed_ms=elapsed);
    }
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.finish("cancelled");
    }
}

fn clean_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.kill_on_drop(true);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("LLAMA_")
            || name.starts_with("GGML_")
            || matches!(name.as_ref(), "HF_TOKEN" | "HUGGING_FACE_HUB_TOKEN")
        {
            command.env_remove(key);
        }
    }
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
    }
    command
}
struct GpuDevice {
    id: String,
    name: String,
}
async fn find_gpu(exe: &Path, cancel: &CancellationToken) -> Option<GpuDevice> {
    let mut command = clean_command(exe);
    command.arg("--list-devices");
    let output = tokio::select! {_=cancel.cancelled()=>return None,result=tokio::time::timeout(Duration::from_secs(15),command.output())=>result.ok()?.ok()?};
    if !output.status.success() {
        return None;
    }
    parse_devices(&String::from_utf8_lossy(&output.stdout))
}
fn parse_devices(text: &str) -> Option<GpuDevice> {
    let mut devices: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let (id, name) = line.trim().split_once(": ")?;
            if !id.starts_with("Vulkan") || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            Some(GpuDevice {
                id: id.into(),
                name: name
                    .split(" (")
                    .next()
                    .unwrap_or(name)
                    .chars()
                    .take(160)
                    .collect(),
            })
        })
        .collect();
    devices.sort_by_key(|d| {
        if d.name.contains("NVIDIA") || d.name.contains("Radeon") || d.name.contains("AMD") {
            0
        } else {
            1
        }
    });
    devices.into_iter().next()
}
async fn validate_file(path: &Path) -> Result<()> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| AppError::new("model_missing", "找不到模型文件或没有读取权限。"))?;
    if !file.metadata().await.map_err(|_| AppError::io())?.is_file() {
        return Err(AppError::invalid("模型路径不是普通文件。"));
    }
    let mut header = [0u8; 24];
    file.read_exact(&mut header)
        .await
        .map_err(|_| AppError::new("model_corrupt", "模型文件不完整，请重新下载。"))?;
    if &header[..4] != b"GGUF"
        || !matches!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 2 | 3)
    {
        return Err(AppError::new(
            "model_format",
            "仅支持有效的 GGUF v2/v3 模型，AWQ 文件不能直接加载。",
        ));
    }
    Ok(())
}
struct NativeProcess {
    child: Child,
    reader: tokio::task::JoinHandle<()>,
    oom: Arc<AtomicBool>,
    _job: JobGuard,
}
impl NativeProcess {
    fn new(mut child: Child) -> Result<Self> {
        let job = JobGuard::attach(&child)?;
        let oom = Arc::new(AtomicBool::new(false));
        let flag = oom.clone();
        let stderr = child.stderr.take();
        let reader = tokio::spawn(async move {
            if let Some(mut stderr) = stderr {
                let mut buf = [0u8; 8192];
                let mut tail = String::new();
                while let Ok(n) = stderr.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    tail.push_str(&String::from_utf8_lossy(&buf[..n]).to_lowercase());
                    if tail.contains("out of memory")
                        || tail.contains("failed to allocate")
                        || tail.contains("not enough memory")
                    {
                        flag.store(true, Ordering::Relaxed);
                    }
                    if tail.len() > 16384 {
                        tail = tail
                            .chars()
                            .rev()
                            .take(512)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                    }
                }
            }
        });
        Ok(Self {
            child,
            reader,
            oom,
            _job: job,
        })
    }
    async fn stop(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.reader.abort();
    }
}
impl Drop for NativeProcess {
    fn drop(&mut self) {
        self.reader.abort();
        let _ = self.child.start_kill();
    }
}
#[cfg(windows)]
struct JobGuard(isize);
#[cfg(windows)]
impl JobGuard {
    fn attach(child: &Child) -> Result<Self> {
        use windows_sys::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(AppError::new("process_guard", "无法创建推理进程资源保护。"));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            );
            let handle = child.raw_handle().ok_or_else(unavailable)?;
            if configured == 0 || AssignProcessToJobObject(job, handle as _) == 0 {
                CloseHandle(job);
                return Err(AppError::new(
                    "process_guard",
                    "无法保护推理子进程，请检查系统进程限制。",
                ));
            }
            Ok(Self(job as isize))
        }
    }
}
#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}
#[cfg(not(windows))]
struct JobGuard;
#[cfg(not(windows))]
impl JobGuard {
    fn attach(_: &Child) -> Result<Self> {
        Ok(Self)
    }
}

struct Validated {
    body: Value,
    streaming: bool,
    dimensions: Option<usize>,
    base64: bool,
}
pub fn validate_api_body(kind: ModelKind, body: &Value) -> Result<()> {
    let model = body["model"]
        .as_str()
        .ok_or_else(|| AppError::invalid("model 必须是字符串。"))?;
    validate_request(kind, body.clone(), model, 131072).map(|_| ())
}
fn validate_request(kind: ModelKind, body: Value, model: &str, context: u32) -> Result<Validated> {
    let object = body
        .as_object()
        .ok_or_else(|| AppError::invalid("请求必须为 JSON 对象。"))?;
    let requested = object
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::invalid("缺少模型名称。"))?;
    if requested.is_empty() || requested.len() > 200 {
        return Err(AppError::invalid("模型名称无效。"));
    }
    if requested != model {
        return Err(AppError::new(
            "model_not_found",
            "请求模型与当前加载的模型不匹配。",
        ));
    }
    let mut clean = json!({"model":model});
    let mut total = 0usize;
    let mut dimensions = None;
    let mut base64 = false;
    let streaming = match object.get("stream") {
        None => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| AppError::invalid("stream 必须是布尔值。"))?,
    };
    if kind == ModelKind::Generation {
        let messages = object
            .get("messages")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or_else(|| AppError::invalid("messages 需要 1–256 条文本消息。"))?;
        let mut output = vec![];
        for message in messages {
            let role = message["role"]
                .as_str()
                .filter(|r| matches!(*r, "system" | "developer" | "user" | "assistant"))
                .ok_or_else(|| {
                    AppError::invalid("只支持 system、developer、user、assistant 文本消息。")
                })?;
            let content = if let Some(text) = message["content"].as_str() {
                text.to_owned()
            } else if let Some(parts) = message["content"].as_array() {
                let mut text = String::new();
                for part in parts {
                    if part["type"] != "text" {
                        return Err(AppError::invalid("此后端只支持文本内容。"));
                    }
                    text.push_str(
                        part["text"]
                            .as_str()
                            .ok_or_else(|| AppError::invalid("文本内容无效。"))?,
                    );
                }
                text
            } else {
                return Err(AppError::invalid("消息内容必须为文本。"));
            };
            total += content.len();
            output.push(json!({"role":role,"content":content}));
        }
        clean["messages"] = json!(output);
        clean["stream"] = json!(streaming);
        for (field, default, min, max) in [
            ("temperature", 0.7, 0.0, 2.0),
            ("top_p", 0.9, f64::EPSILON, 1.0),
            ("presence_penalty", 0.0, -2.0, 2.0),
            ("frequency_penalty", 0.0, -2.0, 2.0),
        ] {
            let value = match object.get(field) {
                Some(v) => v
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= min && *n <= max)
                    .ok_or_else(|| AppError::invalid(format!("{field} 超出有效范围。")))?,
                None => default,
            };
            clean[field] = json!(value);
        }
        let maximum = object
            .get("max_tokens")
            .or_else(|| object.get("max_completion_tokens"))
            .map(|v| v.as_u64())
            .unwrap_or(Some(256))
            .filter(|n| *n >= 1 && *n <= 32768 && *n <= context as u64)
            .ok_or_else(|| {
                AppError::invalid("输出 Token 上限须为 1–32768，且不得超过模型上下文。")
            })?;
        clean["max_tokens"] = json!(maximum);
        if let Some(n) = object.get("n") {
            if n.as_u64() != Some(1) {
                return Err(AppError::invalid("当前服务仅支持 n=1。"));
            }
        }
        if let Some(seed) = object.get("seed") {
            clean["seed"] = json!(seed
                .as_i64()
                .filter(|s| *s >= -1 && *s <= u32::MAX as i64)
                .ok_or_else(|| AppError::invalid("seed 无效。"))?);
        }
        if let Some(stop) = object.get("stop").filter(|v| !v.is_null()) {
            let stops = if let Some(s) = stop.as_str() {
                vec![s]
            } else {
                stop.as_array()
                    .filter(|v| v.len() <= 8)
                    .ok_or_else(|| AppError::invalid("stop 必须是文本或最多 8 个文本的数组。"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .ok_or_else(|| AppError::invalid("stop 内容无效。"))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if stops.iter().any(|s| s.is_empty() || s.len() > 256) {
                return Err(AppError::invalid("stop 内容长度须为 1–256 字节。"));
            }
            clean["stop"] = stop.clone();
        }
        if object.contains_key("tools") || object.contains_key("functions") {
            return Err(AppError::invalid("此版本不提供工具调用，仅支持文本生成。"));
        }
        if let Some(format) = object.get("response_format") {
            if !matches!(format["type"].as_str(), Some("text" | "json_object")) {
                return Err(AppError::invalid("仅支持 text 或 json_object 响应格式。"));
            }
            clean["response_format"] = format.clone();
        }
        if streaming {
            let include = match object.get("stream_options") {
                None => true,
                Some(v) => v
                    .get("include_usage")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        AppError::invalid("stream_options.include_usage 必须为布尔值。")
                    })?,
            };
            clean["stream_options"] = json!({"include_usage":include});
        }
    } else {
        if streaming {
            return Err(AppError::invalid("Embedding 不支持流式响应。"));
        }
        let input = object
            .get("input")
            .ok_or_else(|| AppError::invalid("缺少 input 文本。"))?;
        let texts = if let Some(text) = input.as_str() {
            vec![text]
        } else {
            input
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= 32)
                .ok_or_else(|| AppError::invalid("input 需为文本或 1–32 条文本数组。"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| AppError::invalid("Embedding 输入只支持文本。"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        if texts.iter().any(|s| s.trim().is_empty()) {
            return Err(AppError::invalid("Embedding 文本不能为空。"));
        }
        total = texts.iter().map(|s| s.len()).sum();
        clean["input"] = input.clone();
        clean["encoding_format"] = json!("float");
        if let Some(encoding) = object.get("encoding_format") {
            match encoding.as_str() {
                Some("float") => {}
                Some("base64") => base64 = true,
                _ => {
                    return Err(AppError::invalid(
                        "encoding_format 仅支持 float 或 base64。",
                    ))
                }
            }
        }
        if let Some(d) = object.get("dimensions") {
            dimensions = Some(
                d.as_u64()
                    .filter(|n| *n > 0 && *n <= 65536)
                    .ok_or_else(|| AppError::invalid("dimensions 超出有效范围。"))?
                    as usize,
            );
        }
    }
    if total == 0 || total > MAX_INPUT {
        return Err(AppError::invalid("输入文本总大小须为 1–524288 字节。"));
    }
    Ok(Validated {
        body: clean,
        streaming,
        dimensions,
        base64,
    })
}
fn sanitize_chat(value: Value, stream: bool, model: &str) -> Value {
    let mut clean = json!({"id":value["id"],"object":if stream{"chat.completion.chunk"}else{"chat.completion"},"created":value["created"],"model":model,"choices":value["choices"]});
    if value.get("usage").is_some() {
        clean["usage"] = value["usage"].clone();
    }
    clean
}
fn sanitize_embeddings(
    value: Value,
    model: &str,
    dimensions: Option<usize>,
    base64: bool,
) -> Result<Value> {
    use base64::Engine;
    let data = value["data"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 32)
        .ok_or_else(invalid_response)?;
    let mut output = vec![];
    for (i, item) in data.iter().enumerate() {
        let raw = item["embedding"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 65536)
            .ok_or_else(invalid_response)?;
        let size = dimensions.unwrap_or(raw.len());
        if size > raw.len() {
            return Err(AppError::invalid("dimensions 不能超过模型的原始向量维度。"));
        }
        let mut vector = raw
            .iter()
            .take(size)
            .map(|n| {
                n.as_f64()
                    .filter(|n| n.is_finite())
                    .map(|n| n as f32)
                    .ok_or_else(invalid_response)
            })
            .collect::<Result<Vec<_>>>()?;
        let norm = vector
            .iter()
            .map(|v| (*v as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err(invalid_response());
        }
        for v in &mut vector {
            *v = (*v as f64 / norm) as f32;
        }
        let embedding = if base64 {
            let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            json!(base64::engine::general_purpose::STANDARD.encode(bytes))
        } else {
            json!(vector)
        };
        output.push(json!({"object":"embedding","index":i,"embedding":embedding}));
    }
    let tokens = value["usage"]["prompt_tokens"]
        .as_u64()
        .ok_or_else(invalid_response)?;
    Ok(
        json!({"object":"list","model":model,"data":output,"usage":{"prompt_tokens":tokens,"total_tokens":tokens}}),
    )
}
fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buffer.len().saturating_sub(1) {
        if buffer[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
        if buffer[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chat() -> Value {
        json!({"model":"test","messages":[{"role":"user","content":"hello"}]})
    }
    #[test]
    fn validates_generation_inputs() {
        validate_request(ModelKind::Generation, chat(), "test", 4096).unwrap();
        for (key, value) in [
            ("temperature", json!(-1)),
            ("top_p", json!(0)),
            ("max_tokens", json!(0)),
            ("stream", json!("yes")),
            ("messages", json!([])),
            ("n", json!(2)),
        ] {
            let mut v = chat();
            v[key] = value;
            assert!(
                validate_request(ModelKind::Generation, v, "test", 4096).is_err(),
                "{key}"
            );
        }
        assert!(validate_request(ModelKind::Generation, chat(), "different", 4096).is_err());
    }
    #[test]
    fn rejects_nontext_and_bounds() {
        let mut v = chat();
        v["messages"][0]["content"] = json!([{"type":"image_url","image_url":"file:///secret"}]);
        assert!(validate_request(ModelKind::Generation, v, "test", 4096).is_err());
        assert!(validate_request(
            ModelKind::Embedding,
            json!({"model":"test","input":[1,2]}),
            "test",
            4096
        )
        .is_err());
        assert!(validate_request(
            ModelKind::Embedding,
            json!({"model":"test","input":"x","stream":true}),
            "test",
            4096
        )
        .is_err());
    }
    #[test]
    fn embedding_dimension_normalization_base64() {
        let input = json!({"data":[{"embedding":[3.,4.,5.]}],"usage":{"prompt_tokens":2}});
        let output = sanitize_embeddings(input.clone(), "model", Some(2), false).unwrap();
        assert!((output["data"][0]["embedding"][0].as_f64().unwrap() - 0.6).abs() < 1e-6);
        assert!(sanitize_embeddings(input.clone(), "model", Some(4), false).is_err());
        let b64 = sanitize_embeddings(input, "model", Some(2), true).unwrap();
        assert!(b64["data"][0]["embedding"].is_string());
    }
    #[test]
    fn selects_discrete_gpu() {
        assert_eq!(
            parse_devices("  Vulkan0: Intel(R) GPU (32 MiB)\n  Vulkan1: NVIDIA RTX (8 MiB)")
                .unwrap()
                .id,
            "Vulkan1"
        );
        assert!(parse_devices("No GPU available").is_none());
    }
    #[test]
    fn sse_delimiters() {
        assert_eq!(frame_boundary(b"data: {}\r\n\r\n"), Some((8, 4)));
        assert_eq!(frame_boundary(b"data: {}\n\n"), Some((8, 2)));
        assert_eq!(frame_boundary(b"data: {}\n"), None);
    }
    #[tokio::test]
    async fn missing_and_corrupt_models_return_safe_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            validate_file(&dir.path().join("secret.gguf"))
                .await
                .unwrap_err()
                .code,
            "model_missing"
        );
        let path = dir.path().join("bad.gguf");
        tokio::fs::write(&path, b"not a valid model file at all")
            .await
            .unwrap();
        let e = validate_file(&path).await.unwrap_err();
        assert_eq!(e.code, "model_format");
        assert!(!e.message.contains("bad.gguf"));
    }
    #[tokio::test]
    async fn unloaded_and_shutdown_models_reject_requests() {
        let engine = EngineManager::new(PathBuf::new());
        assert!(engine
            .infer(
                ModelKind::Generation,
                chat(),
                "test".into(),
                ServiceSettings::default()
            )
            .await
            .is_err());
        engine.unload(ModelKind::Generation).await.unwrap();
        engine.shutdown().await;
        engine.shutdown().await;
    }
}
