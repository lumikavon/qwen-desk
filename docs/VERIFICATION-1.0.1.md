# Qwen Desk 1.0.1 自动启动功能验证

新增“启动后自动加载模型并启动 API 服务”开关，应用设置和服务设置共用 `service.auto_start`。默认关闭；保存后在下次普通启动、开机启动或静默启动时执行。自动加载每类首个已有本地模型，不下载权重。

## 自动回归

- `npm test`：2 项 SSE 测试通过。
- `npm run build`：TypeScript 检查和前端生产构建通过。
- `cargo test --locked --manifest-path src-tauri/Cargo.toml --no-default-features`：37 项测试通过，包含 7 项新增的启动功能测试。
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check` 和 `git diff --check`：通过。

新增测试覆盖默认关闭、旧配置缺失字段兼容、启用后保存并重新创建应用核心、没有本地模型时真实 `/health` 可达、API 在模型加载等待期间可达、两类模型均被尝试加载、无效 GGUF 错误可见、模型失败后继续另一类、端口冲突及关闭时延迟启动不能重新监听。夹具模型只用于失败路径，不作为真实推理成功证据。

## 真实模型与 API

另以临时独立配置和端口，引用本机已有的两个默认 GGUF 文件，使用仓库固定版本的 CPU 运行库执行完整启动验证。模型版本和 SHA-256 见 `models.lock.json`。推理配置为上下文 512、批处理 64、CPU 线程 4。

先保存启用自动启动的配置，关闭并重新创建 `AppCore`，调用正式的 `auto_start()`。两个模型均进入 `loaded` 状态，`/health` 和 `/v1/models` 返回成功；随后通过真实聊天和 Embedding HTTP 请求确认引擎可用。验证结束调用正常关闭流程。

```text
AUTOLOAD generation: PASS
AUTOLOAD embedding: PASS
REAL_API chat/completions: PASS
REAL_API embeddings: PASS
PERSISTED_STARTUP_HEALTH: PASS
```

此项验证没有修改用户应用数据目录的配置，也未下载新的模型。

## 设置界面

浏览器中使用正式 `settingsForm` 组件的独立检查页，验证应用设置和服务设置均显示同一开关、默认未选中、已启用状态可恢复、鼠标与空格键可切换，以及可访问名称和说明文字的关联。这里只验证表单组件交互，配置持久化与启动行为由上述后端及真实模型验证覆盖。仅改变启动开关不会提示重启当前 API；监听地址、端口、并发或超时变化仍按原流程提示重启。
