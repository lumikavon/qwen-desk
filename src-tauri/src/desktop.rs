use crate::{
    config::{ModelKind, ModelSpec, Paths, Settings},
    error::{AppError, Result},
    AppCore,
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State,
};
#[cfg(not(windows))]
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_dialog::DialogExt;
use tracing_subscriber::prelude::*;

#[cfg(all(windows, debug_assertions))]
#[path = "devtools.rs"]
mod devtools;
#[cfg(windows)]
#[path = "native_windows.rs"]
mod native_windows;
use tauri_plugin_window_state::{StateFlags, WindowExt};
struct DesktopState {
    close_to_tray: AtomicBool,
    quitting: AtomicBool,
    shutdown_done: AtomicBool,
}
struct LogControl {
    update: Box<dyn Fn(&str) + Send + Sync>,
    _guard: std::sync::Mutex<tracing_appender::non_blocking::WorkerGuard>,
}
struct TrayLabels {
    show: MenuItem<tauri::Wry>,
    start: MenuItem<tauri::Wry>,
    stop: MenuItem<tauri::Wry>,
    quit: MenuItem<tauri::Wry>,
}
impl TrayLabels {
    fn update(&self, app: &tauri::AppHandle, language: &str) -> Result<()> {
        let en = language == "en";
        for (item, zh, english) in [
            (&self.show, "打开 Qwen Desk", "Open Qwen Desk"),
            (&self.start, "启动 API 服务", "Start API Service"),
            (&self.stop, "停止 API 服务", "Stop API Service"),
            (&self.quit, "退出", "Quit"),
        ] {
            item.set_text(if en { english } else { zh })
                .map_err(|_| native_error())?;
        }
        if let Some(tray) = app.tray_by_id("main") {
            tray.set_tooltip(Some(if en {
                "Qwen Desk · Local inference"
            } else {
                "Qwen Desk · 本地推理"
            }))
            .map_err(|_| native_error())?;
        }
        Ok(())
    }
}
fn native_error() -> AppError {
    AppError::new("window_error", "窗口操作失败，请重试。")
}
fn field<'a>(payload: &'a Value, name: &str) -> Result<&'a str> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 4096)
        .ok_or_else(|| AppError::invalid("缺少有效的操作参数。"))
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| AppError::invalid("参数格式或类型无效。"))
}

#[tauri::command]
async fn dispatch(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    core: State<'_, Arc<AppCore>>,
    op: String,
    payload: Option<Value>,
) -> Result<Value> {
    if window.label() != "main" {
        return Err(AppError::new("forbidden", "不允许此窗口访问应用。"));
    }
    let payload = payload.unwrap_or(Value::Null);
    if op.len() > 64
        || serde_json::to_vec(&payload)
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
            > 2 * 1024 * 1024
    {
        return Err(AppError::invalid("请求过大。"));
    }
    core.ensure_running()?;
    match op.as_str() {
        "snapshot" => Ok(core.snapshot().await),
        "save_hf_token" => {
            core.downloads
                .save_credential(field(&payload, "token")?)
                .await?;
            Ok(json!(true))
        }
        "save_settings" => {
            let _library = core.library_gate.lock().await;
            let _gate = core.settings_gate.lock().await;
            let settings: Settings = parse(payload)?;
            settings.validate()?;
            let old = core.settings.read().await.clone();
            core.validate_model_changes(&settings).await?;
            let app2 = app.clone();
            let enabled = settings.app.autostart;
            tauri::async_runtime::spawn_blocking(move || set_autostart(&app2, enabled))
                .await
                .map_err(|_| AppError::new("autostart_error", "无法更新开机启动设置。"))?
                .map_err(|_| {
                    AppError::new("autostart_error", "无法更新开机启动，请检查系统权限。")
                })?;
            if let Some(tray) = app.tray_by_id("main") {
                if tray.set_visible(settings.app.tray_enabled).is_err() {
                    restore_autostart(&app, old.app.autostart).await;
                    return Err(AppError::new("tray_error", "无法更新托盘图标。"));
                }
            }
            let saved = match core.persist_settings(settings).await {
                Ok(s) => s,
                Err(e) => {
                    restore_autostart(&app, old.app.autostart).await;
                    if let Some(t) = app.tray_by_id("main") {
                        let _ = t.set_visible(old.app.tray_enabled);
                    }
                    return Err(e);
                }
            };
            app.state::<DesktopState>().close_to_tray.store(
                saved.app.close_to_tray && saved.app.tray_enabled,
                Ordering::Release,
            );
            (app.state::<LogControl>().update)(&saved.app.log_level);
            app.state::<TrayLabels>()
                .update(&app, &saved.app.language)?;
            Ok(json!(saved))
        }
        "add_model" => {
            let model: ModelSpec = parse(
                payload
                    .get("model")
                    .cloned()
                    .ok_or_else(|| AppError::invalid("缺少模型配置。"))?,
            )?;
            core.add_model(model).await?;
            Ok(json!(true))
        }
        "remove_model" => {
            core.remove_model(field(&payload, "id")?).await?;
            Ok(json!(true))
        }
        "download" => {
            core.download(field(&payload, "id")?).await?;
            Ok(json!(true))
        }
        "cancel_download" => {
            core.downloads.cancel(field(&payload, "id")?).await;
            Ok(json!(true))
        }
        "delete" => {
            core.delete(field(&payload, "id")?).await?;
            Ok(json!(true))
        }
        "load" => {
            core.load(field(&payload, "id")?).await?;
            Ok(json!(true))
        }
        "unload" => {
            let kind: ModelKind = parse(payload["kind"].clone())?;
            core.engine.unload(kind).await?;
            Ok(json!(true))
        }
        "service" => {
            core.service_action(field(&payload, "action")?).await?;
            Ok(json!(true))
        }
        "cancel_inference" => {
            core.engine.cancel(field(&payload, "request_id")?).await;
            Ok(json!(true))
        }
        "run_inference" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Input {
                kind: ModelKind,
                request_id: String,
                body: Value,
            }
            let input: Input = parse(payload)?;
            let streaming = input.body["stream"].as_bool().unwrap_or(false);
            let limits = core.settings.read().await.service.clone();
            let mut response = core
                .engine
                .infer(input.kind, input.body, input.request_id.clone(), limits)
                .await?;
            let mut buffer = Vec::new();
            while let Some(chunk) = response.stream.next().await {
                buffer.extend_from_slice(&chunk?);
                if buffer.len() > 32 * 1024 * 1024 {
                    return Err(AppError::new(
                        "response_too_large",
                        "响应超过大小限制，请减少输入数量。",
                    ));
                }
                if streaming {
                    let valid = match std::str::from_utf8(&buffer) {
                        Ok(_) => buffer.len(),
                        Err(e) if e.error_len().is_none() => e.valid_up_to(),
                        Err(_) => {
                            return Err(AppError::new("invalid_response", "推理引擎返回无效文本。"))
                        }
                    };
                    if valid > 0 {
                        let text = std::str::from_utf8(&buffer[..valid])
                            .map_err(|_| {
                                AppError::new("invalid_response", "推理引擎返回无效文本。")
                            })?
                            .to_owned();
                        window
                            .emit(
                                "inference_chunk",
                                json!({"request_id":input.request_id,"chunk":text}),
                            )
                            .map_err(|_| native_error())?;
                        buffer.drain(..valid);
                    }
                }
            }
            if streaming {
                if !buffer.is_empty() {
                    return Err(AppError::new("invalid_response", "推理流意外中断。"));
                }
                Ok(json!({"done":true}))
            } else {
                serde_json::from_slice(&buffer)
                    .map_err(|_| AppError::new("invalid_response", "推理引擎响应格式无效。"))
            }
        }
        "choose_model_file" => {
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.dialog()
                .file()
                .add_filter(
                    if core.settings.read().await.app.language == "en" {
                        "GGUF model"
                    } else {
                        "GGUF 模型"
                    },
                    &["gguf"],
                )
                .pick_file(move |path| {
                    let _ = tx.send(path);
                });
            let selected = rx
                .await
                .map_err(|_| AppError::new("dialog_error", "文件选择窗口已关闭。"))?;
            match selected {
                Some(path) => Ok(json!(path
                    .into_path()
                    .map_err(|_| AppError::invalid("请选择本地 GGUF 文件。"))?
                    .to_string_lossy())),
                None => Ok(Value::Null),
            }
        }
        "open_logs" => {
            let path = core.paths.logs.clone();
            tauri::async_runtime::spawn_blocking(move || {
                std::process::Command::new("explorer.exe").arg(path).spawn()
            })
            .await
            .map_err(|_| AppError::io())?
            .map_err(|_| AppError::io())?;
            Ok(json!(true))
        }
        "window" => {
            match field(&payload, "action")? {
                "minimize" => window.minimize().map_err(|_| native_error())?,
                "maximize" => {
                    if window.is_maximized().map_err(|_| native_error())? {
                        window.unmaximize()
                    } else {
                        window.maximize()
                    }
                    .map_err(|_| native_error())?;
                }
                "close" => window.close().map_err(|_| native_error())?,
                "show" => show_window(&app),
                "devtools" => {
                    #[cfg(all(windows, debug_assertions))]
                    {
                        let closed = tauri::async_runtime::spawn_blocking(devtools::close_if_open)
                            .await
                            .map_err(|_| native_error())??;
                        if !closed {
                            window.open_devtools();
                        }
                    }
                    #[cfg(all(not(windows), debug_assertions))]
                    {
                        if window.is_devtools_open() {
                            window.close_devtools();
                        } else {
                            window.open_devtools();
                        }
                    }
                    #[cfg(not(debug_assertions))]
                    {
                        return Err(AppError::new("not_available", "生产版本不提供开发工具。"));
                    }
                }
                _ => return Err(AppError::invalid("未知窗口操作。")),
            }
            Ok(json!(true))
        }
        _ => Err(AppError::invalid("未知操作。")),
    }
}
fn set_autostart(_app: &tauri::AppHandle, enabled: bool) -> Result<()> {
    #[cfg(windows)]
    {
        native_windows::set_autostart(enabled)
    }
    #[cfg(not(windows))]
    {
        let manager = _app.autolaunch();
        (if enabled {
            manager.enable()
        } else {
            manager.disable()
        })
        .map_err(|_| AppError::new("autostart_error", "无法更新开机启动设置。"))
    }
}
fn restore_main_window(app: &tauri::AppHandle) -> Result<()> {
    use std::io::Read;
    let window = app.get_webview_window("main").ok_or_else(native_error)?;
    let saved = app
        .path()
        .app_config_dir()
        .ok()
        .and_then(|p| std::fs::File::open(p.join(tauri_plugin_window_state::DEFAULT_FILENAME)).ok())
        .and_then(|file| {
            let mut bytes = Vec::new();
            file.take(65537).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 65536 {
                return None;
            }
            serde_json::from_slice::<Value>(&bytes).ok()
        });
    if let Some(state) = saved.as_ref().and_then(|v| v.get("main")) {
        let valid = state["width"]
            .as_u64()
            .is_some_and(|v| (320..=16384).contains(&v))
            && state["height"]
                .as_u64()
                .is_some_and(|v| (240..=16384).contains(&v))
            && state["x"]
                .as_i64()
                .is_some_and(|v| (-1_000_000..=1_000_000).contains(&v))
            && state["y"]
                .as_i64()
                .is_some_and(|v| (-1_000_000..=1_000_000).contains(&v));
        if !valid {
            return Ok(());
        }
    }
    window
        .restore_state(StateFlags::SIZE | StateFlags::POSITION)
        .map_err(|_| native_error())?;
    #[cfg(windows)]
    if let Some(state) = saved.as_ref().and_then(|v| v.get("main")) {
        if let (Some(width), Some(height)) = (state["width"].as_u64(), state["height"].as_u64()) {
            if (320..=16384).contains(&width) && (240..=16384).contains(&height) {
                let mut width = width as u32;
                let mut height = height as u32;
                if let Some(monitor) = window.current_monitor().map_err(|_| native_error())? {
                    width = width.min(monitor.work_area().size.width.saturating_sub(24));
                    height = height.min(monitor.work_area().size.height.saturating_sub(24));
                }
                native_windows::restore_client_size(
                    window.hwnd().map_err(|_| native_error())?.0 as isize,
                    width,
                    height,
                )?;
            }
        }
    }
    if let Some(monitor) = window.current_monitor().map_err(|_| native_error())? {
        let area = monitor.work_area();
        let size = window.outer_size().map_err(|_| native_error())?;
        let position = window.outer_position().map_err(|_| native_error())?;
        let x = position.x.clamp(
            area.position.x,
            area.position.x + area.size.width.saturating_sub(size.width) as i32,
        );
        let y = position.y.clamp(
            area.position.y,
            area.position.y + area.size.height.saturating_sub(size.height) as i32,
        );
        if x != position.x || y != position.y {
            window
                .set_position(tauri::PhysicalPosition::new(x, y))
                .map_err(|_| native_error())?;
        }
    }
    if saved
        .as_ref()
        .is_some_and(|v| v["main"]["maximized"] == true)
    {
        window.maximize().map_err(|_| native_error())?;
    }
    Ok(())
}
async fn restore_autostart(app: &tauri::AppHandle, enabled: bool) {
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || set_autostart(&app, enabled)).await;
}
fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.current_monitor().ok().flatten().is_none() {
            let _ = window.center();
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
fn request_exit(app: &tauri::AppHandle) {
    let state = app.state::<DesktopState>();
    if state.quitting.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    let core = app.state::<Arc<AppCore>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        core.shutdown().await;
        app.state::<DesktopState>()
            .shutdown_done
            .store(true, Ordering::Release);
        app.exit(0);
    });
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|a| a == "--silent" || a == "--autostart") {
                show_window(app);
            }
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .app_name("Qwen Desk")
                .args(["--autostart"])
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .skip_initial_state("main")
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::SIZE
                        | tauri_plugin_window_state::StateFlags::POSITION
                        | tauri_plugin_window_state::StateFlags::MAXIMIZED,
                )
                .build(),
        )
        .setup(|app| {
            let data = app.path().app_local_data_dir()?;
            #[cfg(debug_assertions)]
            let runtime = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
            #[cfg(not(debug_assertions))]
            let runtime = app.path().resource_dir()?.join("runtime");
            let core = tauri::async_runtime::block_on(AppCore::new(Paths::new(data, runtime)))?;
            let settings =
                tauri::async_runtime::block_on(async { core.settings.read().await.clone() });
            let file = tracing_appender::rolling::Builder::new()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("qwen-desk")
                .max_log_files(7)
                .build(&core.paths.logs)?;
            let (writer, guard) = tracing_appender::non_blocking(file);
            let filter = tracing_subscriber::EnvFilter::new(format!(
                "off,qwen_desk={}",
                settings.app.log_level
            ));
            let (filter, handle) = tracing_subscriber::reload::Layer::new(filter);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_target(false)
                        .with_writer(writer),
                )
                .try_init();
            app.manage(LogControl {
                update: Box::new(move |level| {
                    let _ = handle.reload(tracing_subscriber::EnvFilter::new(format!(
                        "off,qwen_desk={level}"
                    )));
                }),
                _guard: std::sync::Mutex::new(guard),
            });
            app.manage(DesktopState {
                close_to_tray: AtomicBool::new(
                    settings.app.close_to_tray && settings.app.tray_enabled,
                ),
                quitting: AtomicBool::new(false),
                shutdown_done: AtomicBool::new(false),
            });
            app.manage(core.clone());
            let show = MenuItem::with_id(app, "show", "打开 Qwen Desk", true, None::<&str>)?;
            let start = MenuItem::with_id(app, "start", "启动 API 服务", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "停止 API 服务", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
            let app_icon = app.default_window_icon().ok_or("missing app icon")?.clone();
            if let Some(window) = app.get_webview_window("main") {
                window.set_icon(app_icon)?;
            }
            // Use the matching 32px raster for a sharp tray icon on high-DPI displays.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
            let tray = TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .tooltip("Qwen Desk · 本地推理")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_window(app),
                    "quit" => request_exit(app),
                    "start" | "stop" => {
                        let core = app.state::<Arc<AppCore>>().inner().clone();
                        let app = app.clone();
                        let action = event.id.as_ref().to_owned();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = core.service_action(&action).await {
                                let _ = app.emit("app_error", e);
                                show_window(&app);
                            }
                        });
                    }
                    _ => (),
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_window(tray.app_handle());
                    }
                })
                .build(app)?;
            tray.set_visible(settings.app.tray_enabled)?;
            app.manage(TrayLabels {
                show,
                start,
                stop,
                quit,
            });
            app.state::<TrayLabels>()
                .update(app.handle(), &settings.app.language)?;
            restore_main_window(app.handle()).map_err(|e| std::io::Error::other(e.message))?;
            // VISIBLE is not restored: a previously hidden window must remain reachable.
            let silent = (settings.app.silent_start || std::env::args().any(|a| a == "--silent"))
                && settings.app.tray_enabled;
            if !silent {
                show_window(app.handle());
            }
            let startup_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let enabled = core.settings.read().await.app.autostart;
                let result = tauri::async_runtime::spawn_blocking(move || {
                    set_autostart(&startup_app, enabled)
                })
                .await;
                if !matches!(result, Ok(Ok(()))) {
                    *core.startup_error.write().await = Some(AppError::new(
                        "autostart_error",
                        "无法恢复开机启动设置，请检查系统权限。",
                    ));
                }
                core.auto_start().await;
            });
            tracing::info!(
                event = "desktop_started",
                version = env!("CARGO_PKG_VERSION")
            );
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app = window.app_handle();
                if app
                    .state::<DesktopState>()
                    .close_to_tray
                    .load(Ordering::Acquire)
                {
                    let _ = window.hide();
                } else {
                    request_exit(app);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![dispatch])
        .build(tauri::generate_context!());
    match app {
        Ok(app) => app.run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !app
                    .state::<DesktopState>()
                    .shutdown_done
                    .load(Ordering::Acquire)
                {
                    api.prevent_exit();
                    request_exit(app);
                }
            }
        }),
        Err(error) => {
            eprintln!("Qwen Desk startup failed: {error}");
        }
    }
}
