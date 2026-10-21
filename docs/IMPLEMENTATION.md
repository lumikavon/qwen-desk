# Qwen Desk implementation contract

## Architecture and boundaries

Qwen Desk is a Windows x64, offline inference desktop product. Tauri 2 owns the native window and tray, a Rust Tokio core owns settings, model downloads, two independent llama.cpp processes and the OpenAI HTTP gateway. Vite/TypeScript renders five Chinese routes. No inference is simulated. Native engine processes are shipped in the installer (CPU and Vulkan), authenticated on ephemeral loopback ports, killed/reaped on unload and shutdown. Models are kept outside the installation in app-local data; HF credentials remain in a private YAML file and never cross IPC.

Default generation: API alias `warshanks/Qwen3-4B-Instruct-2507-AWQ`, actual compatible GGUF `unsloth/Qwen3-4B-Instruct-2507-GGUF/Qwen3-4B-Instruct-2507-Q4_K_M.gguf`. These are distinct quantizations of Qwen3-4B-Instruct-2507; AWQ tensor files are rejected rather than falsely interpreted. The UI discloses the mapping. Default embeddings: alias `Qwen/Qwen3-Embedding-0.6B`, official `Qwen/Qwen3-Embedding-0.6B-GGUF/Qwen3-Embedding-0.6B-Q8_0.gguf`, last-token pooling and normalization.

CPU fallback is automatic after Vulkan launch/load failure. GPU layers, context, batches, CPU threads, KV cache type and GPU memory fit margin are configurable. Each process has a single model lifetime. Per-model transition locks prevent repeated loads racing. A bounded inference gate and per-request cancellation/deadline protect resources. Dropping an HTTP stream closes upstream generation; app quit cancels all work and awaits process termination.

## Stories / acceptance gates (dependency order)

1. Foundation: secure persisted settings, validated model specs and errors; Rust unit tests.
2. Downloads: private YAML resolution, authenticated pinned HF metadata, range resume, SHA256 and GGUF magic validation, progress/cancel/retry, safe deletion; tests plus actual default downloads.
3. Inference: packaged engine, model lifecycle, CPU/Vulkan, OpenAI request validation, generation/embedding streams and cancellation; real model tests.
4. HTTP: bounded OpenAI gateway, JSON/SSE, error sanitation, start/stop/restart, changed port; integration tests.
5. Desktop/UI: all five routes, custom chrome, tray, autostart/silent, DevTools debug-only; frontend build and Windows interaction tests.
6. Delivery: debug and release builds; clean install, run, uninstall, reinstall; source archive and NSIS installer; audit all goal.md requirements.

All six acceptance gates have been implemented and exercised. The final checks, actual-model results, installation cycle and environment limits are recorded in [VERIFICATION.md](VERIFICATION.md).

## UI design and prototype

Use a quiet, light workstation interface: cloud #F4F6FA, paper #FFFFFF, ink #203047, muted #5B6B80, cobalt #3159C6, teal #137D76. Segoe UI Variable / Microsoft YaHei system fonts, 15px body, 28px titles, 1.5 line height. Sidebar 208px, custom titlebar 42px, main content aligned left with 28px padding. Structured rows for model library, a wide output workbench for inference, grouped settings forms. Status includes text and icon, keyboard-visible focus, 40px controls. No external fonts or assets. UI Pro Max's developer-tool/real-time monitoring recommendations inform accessibility/status; marketing landing-page suggestions are rejected as irrelevant.

```
[Qwen Desk                    local inference      _ □ ×]
[仪表盘       ][服务 已停止         启动服务             ]
[模型管理     ][生成模型     Qwen3 4B      未加载        ]
[推理测试     ][向量模型     Qwen3 0.6B    未加载        ]
[服务设置     ][最近推理记录 / 初次使用指引               ]
[应用设置     ][API 地址、运行模式、资源状态              ]
```

Model flow: choose default/custom HF file or local GGUF → validate → download with progress/cancel → load → test → unload/delete with confirmation. Generation workbench: parameters + editable prompt, stream output, cancel, elapsed/tokens. Embeddings: multiline input, vector dimension/norm/preview and usage. Settings save validates centrally; service settings require restart with feedback. Every operation has busy/error/success states and meaningful disabled controls.

## Shared Rust interfaces (snake_case serialized)

`AppError { code: String, message: String }`, safe messages only. `Result<T>` alias.
`ModelKind` enum `generation|embedding`, hashable/copy/display.
`ModelSpec { id, name, kind, source_repo, filename, path, compatibility }`.
`InferenceSettings { context_size:u32, batch_size:u32, threads:u32, gpu_layers:i32, gpu_memory_mb:u32, cache_type:String }`.
`ServiceSettings { host:String, port:u16, concurrency:usize, timeout_secs:u64, auto_start:bool }`.
`AppSettings { autostart, silent_start, close_to_tray, tray_enabled:bool, log_level:String }`.
`Settings { models:Vec<ModelSpec>, inference:InferenceSettings, service:ServiceSettings, app:AppSettings }`.
`Paths { data:PathBuf, models:PathBuf, logs:PathBuf, runtime:PathBuf, config:PathBuf, hf_candidates:Vec<PathBuf> }`.

Engine and HTTP module interfaces:
- `EngineManager::new(runtime: PathBuf) -> Arc<Self>`
- `load(&self, model:ModelSpec, params:InferenceSettings) -> Result<()>`, `unload(&self, kind:ModelKind) -> Result<()>`, `shutdown(&self)` async
- `snapshot(&self) -> serde_json::Value` async (object with generation/embedding each: status, model, mode, error, active_requests; plus last_inference)
- `infer(self:&Arc<Self>, kind:ModelKind, body:Value, request_id:String, limits:ServiceSettings) -> Result<Inference>` async
- `cancel(&self, request_id:&str)` async
- `Inference { pub stream: Pin<Box<dyn Stream<Item=Result<bytes::Bytes>> + Send>>, pub content_type:String }`
- `api::start(engine:Arc<EngineManager>, settings:ServiceSettings) -> Result<ApiServer>` async; `ApiServer { address:String, ... }`, `stop(&mut self)` async. Include /health and /v1/models, no unauthenticated mutation/admin endpoints.

Downloader module interfaces:
- `DownloadManager::new(paths:Paths) -> Arc<Self>`
- `credential_status(&self) -> Value` async (safe status/message, never token)
- `start(self:&Arc<Self>, model:ModelSpec) -> Result<()>` async (background transfer)
- `cancel(&self, id:&str)` async; `snapshot(&self) -> Value` async (array records with id,status,downloaded,total,error,path)
- `delete(&self, model:&ModelSpec) -> Result<()>` async (managed files only)
- `shutdown(&self)` async; local model metadata missing/available included by main snapshot.
Credentials lookup starts current working directory huggingface.yml, debug project root, executable adjacent and app data. Never persist tokens. YAML supports token, hf_token, huggingface.token and hf.token. Private HF metadata must not be exposed. Downloads stored `Paths.models/<id>/<filename leaf>`. ID validated [A-Za-z0-9_-], max 64. Repo owner/name, file path validated no absolute/dot/empty components. Existing local model paths can be absolute GGUF files, delete never arbitrary imported file.

Frontend calls Tauri invoke `dispatch({op,payload})`:
- snapshot -> {settings,engine,downloads,service:{running,address},hf:{status,message},paths:{models,logs},version,dev_mode}
- save_settings payload full Settings -> Settings
- download/load/delete {id}; cancel_download {id}; unload {kind}
- service {action:'start'|'stop'|'restart'}
- add_model {model:ModelSpec} (custom id; local path or HF repo/file)
- remove_model {id} unregister custom model only
- cancel_inference {request_id}
- window {action:'minimize'|'maximize'|'close'|'show'|'devtools'}
- open_logs / choose_model_file -> String/null
- run_inference {kind,request_id,body} -> JSON for nonstream; stream returns {done:true}; event `inference_chunk` payload {request_id,chunk:String} raw SSE bytes decoded UTF8 safely.
Polling snapshot every ~1s is permitted; no credentials in events. No browser-only mocks. Outside Tauri show a clear native-runtime-required state.
