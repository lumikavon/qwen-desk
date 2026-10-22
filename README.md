# Qwen Desk

Windows 本地模型工作台，基于 Tauri 2、Rust、llama.cpp 和 TypeScript。支持生成/翻译、Embedding、模型下载及本机 OpenAI 兼容服务。模型推理在本机原生进程执行，前端不接触 Hugging Face 凭据。

## 安装与首次使用

发行版提供 `QwenDesk-1.0.2-x64-no-model-setup.exe`（不含模型）和 `QwenDesk-1.0.2-x64-with-model-setup.exe`（含两个默认模型）。含模型版需将安装程序及所有同名前缀的编号 `.bin` 文件下载到同一目录，再运行 `.exe`。系统需 Windows 10/11 x64、Microsoft Visual C++ 2015–2022 x64 运行库和 Microsoft Edge WebView2；不含模型版在缺少 WebView2 时会下载微软引导程序，含模型版离线安装前请先安装 WebView2。GPU 模式需要显卡厂商提供的 Vulkan 驱动；CPU 引擎一并安装。

若尚未安装 Visual C++ 运行库，先运行微软官方 [x64 Redistributable](https://aka.ms/vs/17/release/vc_redist.x64.exe)；当前用户安装器不会修改系统级 Visual C++ 运行库。

1. 不含模型版若需要在线下载，准备私有的 `huggingface.yml`，使用下文格式；含模型版使用已安装的默认模型无需令牌。
2. 在“应用设置”检查 Hugging Face 配置状态。
3. 在“模型管理”下载所需模型，等待 SHA-256 校验完成，再点击“加载”。也可以添加已有 GGUF 文件或其他兼容的 Hugging Face GGUF 模型。
4. 在“推理测试”运行翻译/生成或向量化，或在“仪表盘”启动 API；“服务设置”用于修改监听地址、端口及资源参数。
5. 默认关闭窗口会隐藏到托盘。从托盘打开窗口或退出；可以在设置中改变关闭行为。
6. 在“应用设置 → 启动与窗口”开启“启动后自动加载模型并启动 API 服务”，然后保存。下次启动会从模型配置中分别加载首个已存在的生成、向量 GGUF，并启动 API。该选项默认关闭，也适用于开机启动和静默启动；不会自动下载模型。API 会先开始监听，模型加载进度可在仪表盘查看。端口冲突或模型加载失败会显示错误，仍可手动处理。

`1.0.1` 自动启动功能的测试范围与真实模型验证结果见 [专项验证记录](docs/VERIFICATION-1.0.1.md)。

`1.0.2` 统一应用、标题栏、侧栏、Windows 任务栏、托盘及安装器的蓝色 Q 图标。`src/logo.svg` 是唯一图标源；修改后运行 `python scripts/create-icons.py` 生成 PNG/ICO，使用 `python scripts/create-icons.py --check` 检查全部尺寸和 ICO 帧是否同步。生成器需要 Pillow。

模型、日志和配置位于 `%LOCALAPPDATA%\com.qwendesk.desktop`。卸载移除程序和本产品的开机启动项，保留个人模型及设置，重新安装后可以继续使用。应用中删除模型仅能删除其自身下载的文件；导入的外部文件不允许通过应用删除。
窗口位置、大小及最大化状态单独保存在 `%APPDATA%\com.qwendesk.desktop\.window-state.json`。恢复时会检查显示器工作区域，避免窗口留在已经移除的显示器之外。

## 默认模型与格式兼容性

| 用途 / API 名称 | 实际运行文件 | 说明 |
| --- | --- | --- |
| `warshanks/Qwen3-4B-Instruct-2507-AWQ` | `unsloth/Qwen3-4B-Instruct-2507-GGUF` / `Qwen3-4B-Instruct-2507-Q4_K_M.gguf` | 使用同源 Qwen3-4B-Instruct-2507 的另一种量化格式，API 名称保留需求中的别名。不是将 AWQ 文件直接重命名，也不声称两种量化权重完全相同。 |
| `Qwen/Qwen3-Embedding-0.6B` | `Qwen/Qwen3-Embedding-0.6B-GGUF` / `Qwen3-Embedding-0.6B-Q8_0.gguf` | 官方 GGUF，last-token pooling 和 L2 归一化，默认 1024 维。 |

本产品以 Rust 管理官方 llama.cpp 原生服务进程，通过私有的、随机密钥保护的回环连接调用推理。这一进程隔离方式保留 llama.cpp 的原生 CPU/Vulkan 执行性能，同时允许取消、独立卸载两个模型及在原生后端崩溃时回收资源。它不依赖 Ollama、Python、云端推理或另行安装的 llama.cpp。

原始 AWQ/Safetensors 文件不能被此后端直接加载，界面会明确提示。替换模型需要 llama.cpp 支持其 GGUF 架构，生成模型还需正确的 chat template；Embedding 模型需支持序列级输出。第三方模型的许可证和下载权限以模型发布者为准。含模型版附带经过 SHA-256 校验的默认模型；不含模型版可自行下载或导入。模型许可与来源见 `docs/MODEL-NOTICES.md`。

## Hugging Face 配置

创建私有文件 `huggingface.yml`：

```yaml
token: "填写您自己的 Hugging Face 只读访问令牌"
```

也支持 `hf_token` 、`huggingface.token` 或 `hf.token` 字段。程序依次检查工作目录的 `./huggingface.yml`、开发时的项目根目录、可执行文件旁、应用数据目录。安装后推荐放在 `%LOCALAPPDATA%\com.qwendesk.desktop\huggingface.yml`，这样从开始菜单或开机启动也能找到。

下载时自动读取文件，凭据不持久化到其他配置、不发送到前端、不写入日志。缺少文件、YAML 无效、401 和 403 分别给出可操作的错误。下载以 Hugging Face 元数据中的不可变提交、文件大小和 SHA-256 固定版本；取消保留可恢复的部分文件，重新下载会校验元数据并续传，完成时校验文件后才显示为可用。修改 token 后无需重启。

不要将真实令牌写进示例、源码、命令行参数或提交。仓库已忽略 `huggingface.yml`、模型权重、日志、构建缓存和本地密钥。

## OpenAI API

默认 `http://127.0.0.1:8848`。支持 `/health`、`GET /v1/models`、`POST /v1/chat/completions` 和 `POST /v1/embeddings`。API 的 `model` 使用模型配置的名称；请先加载相应模型。

```powershell
$chat = @{
  model = 'warshanks/Qwen3-4B-Instruct-2507-AWQ'
  messages = @(@{ role = 'user'; content = 'Translate into English: 你好，世界。' })
  temperature = 0.3
  top_p = 0.9
  max_tokens = 128
  stream = $false
} | ConvertTo-Json -Depth 5
Invoke-RestMethod http://127.0.0.1:8848/v1/chat/completions -Method Post -ContentType 'application/json; charset=utf-8' -Body ([Text.Encoding]::UTF8.GetBytes($chat))

$embedding = @{ model = 'Qwen/Qwen3-Embedding-0.6B'; input = @('本地推理', 'local inference') } | ConvertTo-Json
Invoke-RestMethod http://127.0.0.1:8848/v1/embeddings -Method Post -ContentType 'application/json; charset=utf-8' -Body ([Text.Encoding]::UTF8.GetBytes($embedding))
```

聊天 `stream:true` 返回 OpenAI SSE chunks 和 `data: [DONE]`，客户端断开连接会释放请求。错误统一为 `{"error":{"message":"...","type":"...","code":"..."}}`。输入大小、参数范围、并发和超时均有限制；超额并发不会建立无限队列。设置服务参数后重启服务使其生效，推理引擎参数在下次加载模型时生效。
每种模型同时处理一个请求，全局并发设置是总上限：默认值 2 允许一个生成请求和一个向量请求并行；同一模型忙碌时返回 429。请求体上限为 1 MiB，文本总量上限为 512 KiB，Embedding 批量上限为 32 条。

默认服务只供本机使用。将监听地址改为 `0.0.0.0` 或非回环 IP 会允许网络访问；外部网关需自行提供身份认证、TLS 和访问控制。该服务不暴露模型删除、任意文件读取或应用管理 HTTP 接口。

## 从源码构建

依赖：Node.js 22.6+（建议 24 LTS）、Rust MSVC stable、Visual Studio 2022 C++ Build Tools（含 Windows SDK）、PowerShell、Git、WebView2。已提交 npm/Cargo 锁文件。

```powershell
npm ci
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/prepare-runtime.ps1
npm run tauri -- dev
```

`prepare-runtime.ps1` 下载固定版本 llama.cpp `b11796` 的官方 Windows CPU/Vulkan 包，并校验固定 SHA-256。不需要本机编译 CUDA/Vulkan。运行库放在被 Git 忽略的 `src-tauri/runtime`。

```powershell
npm test
npm run typecheck
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features
npm run tauri -- build --debug
npm run tauri -- build
```

NSIS 安装包输出到 `src-tauri/target/release/bundle/nsis`。生产包嵌入前端静态资源和两个原生运行库，无开发服务器或开发机绝对路径依赖。生产前端没有 DevTools 控件，后端也拒绝打开开发工具。

真实模型验证（会使用应用数据目录中的模型；下载需要真实凭据）：

```powershell
cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --features verification --bin qwen-verify -- download
cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --features verification --bin qwen-verify -- smoke
cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --features verification --bin qwen-verify -- resilience
```

验证命令需要端口 18848、18849、18850 可用。它检查真实翻译、有效向量、JSON/SSE、HTTP 错误、取消、卸载和 CPU 执行；不使用伪造推理结果。完整交付验证结果见 `docs/VERIFICATION.md`。
安装版加载两个默认模型并启动 API 后，可运行 `powershell -File scripts/verify-api.ps1 -Address http://127.0.0.1:8848`，直接检验安装版的 JSON、SSE、向量和结构化错误。`verification` feature 仅用于源码验证，验证程序不会进入正式安装包。

## CI/CD 与发行

GitLab CI 配置位于 `.gitlab-ci.yml`，GitHub 标签发行流程位于 `.github/workflows/release.yml`。两者使用同一套 Windows 构建、模型校验和发布脚本。推送与源码版本一致的 `v1.0.1` 标签后，GitHub Actions 自动构建两个版本并发布 GitHub Release；手动运行流程也可发布已有标签。

```powershell
npm ci
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-release.ps1 -Variant all -Tag v1.0.1
```

构建结果在 `artifacts/release`，包含两个安装程序、含模型版的分卷数据、`models.lock.json`、发行说明及 `SHA256SUMS`。含模型版每个数据卷不超过 1 GiB。构建会自动下载并校验固定版本的 Inno Setup 编译器；模型可通过 `-ModelDirectory` 从本机缓存导入，否则按固定提交下载。切换两种安装器前请先卸载原程序，卸载保留个人模型及设置。GitLab Windows Runner 环境、变量、发布和校验步骤见 [发行指南](docs/RELEASING.md)。

## 故障定位

- 模型缺失：检查模型管理显示的实际路径，重新下载或重新选择本地 GGUF。
- SHA-256 不匹配：部分文件不视为可运行模型；重试下载获取干净数据。
- GPU 加载失败：程序自动尝试 CPU。可将 GPU 层数设为 0 明确使用 CPU，或减小上下文、批量及 KV cache 精度；显存参数表示为运行时保留的显存余量，不是硬性显存配额。
- 端口占用：停止占用该端口的应用或改用其他端口，再启动服务。
- 推理超时：减少输入/输出长度或在服务设置中增加超时。
- 配置损坏：错误会在应用中显示，原文件保留；修复 `settings.json` 或在界面确认并重新保存设置。
- 日志：应用设置可打开日志目录。日志仅保留必要事件和错误码，不记录令牌、提示词或模型文件路径；最近 7 个每日日志保留在本地。

## 源码结构

- `src/`：五页界面、表单校验、流式解析及前端测试。
- `src-tauri/src/config.rs`：配置、默认模型、边界检查与持久化。
- `src-tauri/src/download.rs`：认证下载、恢复、校验和受限删除。
- `src-tauri/src/engine.rs`：原生推理进程、取消、并发和资源生命周期。
- `src-tauri/src/api.rs`：OpenAI HTTP 网关。
- `src-tauri/src/desktop.rs`：Tauri IPC、托盘、窗口、启动项和日志。
- `src-tauri/src/bin/verify.rs`：真实模型验证入口。
- `scripts/`：可复现运行库准备及交付工具。

项目代码采用 MIT 许可证；依赖和运行库遵循其各自许可证，见 `THIRD_PARTY_NOTICES.md`。
