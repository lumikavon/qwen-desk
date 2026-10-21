//! Public OpenAI-compatible gateway; native engine ports and secrets stay private.
use crate::{
    config::{ModelKind, ServiceSettings},
    engine::{validate_api_body, EngineManager},
    error::{AppError, Result},
};
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    convert::Infallible,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct HttpState {
    engine: Arc<EngineManager>,
    settings: ServiceSettings,
    shutdown: CancellationToken,
    readers: Arc<Semaphore>,
}
pub struct ApiServer {
    pub address: String,
    shutdown: CancellationToken,
    task: Option<JoinHandle<()>>,
}
impl ApiServer {
    pub async fn stop(&mut self) {
        self.shutdown.cancel();
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
                let _ = task.await;
            }
        }
    }
    pub fn is_running(&self) -> bool {
        !self.shutdown.is_cancelled() && self.task.as_ref().is_some_and(|t| !t.is_finished())
    }
}
impl Drop for ApiServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
pub async fn start(engine: Arc<EngineManager>, settings: ServiceSettings) -> Result<ApiServer> {
    settings.validate()?;
    let address = SocketAddr::new(
        settings
            .host
            .parse::<IpAddr>()
            .map_err(|_| AppError::invalid("监听 IP 无效。"))?,
        settings.port,
    );
    let listener = tokio::net::TcpListener::bind(address).await.map_err(|_| {
        AppError::new(
            "port_unavailable",
            "无法监听此地址或端口，可能已被占用或没有权限。",
        )
    })?;
    let address = listener
        .local_addr()
        .map_err(|_| AppError::new("server_error", "无法读取监听地址。"))?;
    let shutdown = CancellationToken::new();
    let state = HttpState {
        engine,
        readers: Arc::new(Semaphore::new((settings.concurrency * 2).clamp(2, 32))),
        settings,
        shutdown: shutdown.clone(),
    };
    let app = router(state);
    let stop = shutdown.clone();
    let task = tokio::spawn(async move {
        if axum::serve(listener, app)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
            .is_err()
        {
            tracing::error!(event = "http_server_stopped");
        }
    });
    Ok(ApiServer {
        address: format!("http://{address}"),
        shutdown,
        task: Some(task),
    })
}
fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/embeddings", post(embeddings))
        .fallback(|| async {
            error_response(
                StatusCode::NOT_FOUND,
                AppError::new("not_found", "接口不存在。"),
            )
        })
        .method_not_allowed_fallback(|| async {
            error_response(
                StatusCode::METHOD_NOT_ALLOWED,
                AppError::new("method_not_allowed", "此接口不支持该 HTTP 方法。"),
            )
        })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            protect_origin,
        ))
        .with_state(state)
}
async fn protect_origin(
    State(state): State<HttpState>,
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    if state.shutdown.is_cancelled() {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            AppError::new("service_stopped", "服务正在停止。"),
        );
    }
    if request.headers().contains_key(header::ORIGIN) {
        // The desktop communicates through IPC; arbitrary websites cannot spend local GPU resources.
        return error_response(
            StatusCode::FORBIDDEN,
            AppError::new(
                "origin_forbidden",
                "不允许跨来源浏览器请求，请使用本机 HTTP 客户端。",
            ),
        );
    }
    if state
        .settings
        .host
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
    {
        if let Some(host) = request.headers().get(header::HOST) {
            let host = host.to_str().unwrap_or("");
            let port = state.settings.port;
            if ![
                format!("127.0.0.1:{port}"),
                format!("localhost:{port}"),
                format!("[::1]:{port}"),
            ]
            .iter()
            .any(|allowed| host.eq_ignore_ascii_case(allowed))
            {
                return error_response(
                    StatusCode::FORBIDDEN,
                    AppError::new("host_forbidden", "监听主机不匹配。"),
                );
            }
        }
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        header::HeaderValue::from_static("nosniff"),
    );
    response
}
async fn health() -> Json<Value> {
    Json(json!({"status":"ok","service":"Qwen Desk"}))
}
async fn models(State(state): State<HttpState>) -> Json<Value> {
    Json(state.engine.models().await)
}
async fn chat(State(state): State<HttpState>, request: Request) -> Response {
    infer(state, ModelKind::Generation, request).await
}
async fn embeddings(State(state): State<HttpState>, request: Request) -> Response {
    infer(state, ModelKind::Embedding, request).await
}
async fn infer(state: HttpState, kind: ModelKind, request: Request) -> Response {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if !content_type.eq_ignore_ascii_case("application/json") {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            AppError::invalid("Content-Type 必须是 application/json。"),
        );
    }
    let permit = match state.readers.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return respond_error(AppError::new(
                "rate_limit_exceeded",
                "服务正在处理其他请求，请稍后重试。",
            ))
        }
    };
    let body = tokio::select! {biased;_=state.shutdown.cancelled()=>return respond_error(AppError::new("service_stopped","服务正在停止。")),result=tokio::time::timeout(Duration::from_secs(10),to_bytes(request.into_body(),1024*1024))=>result};
    let bytes = match body {
        Ok(Ok(b)) => b,
        Ok(Err(_)) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                AppError::new("request_too_large", "请求正文无法读取或超过 1 MiB。"),
            )
        }
        Err(_) => {
            return error_response(
                StatusCode::REQUEST_TIMEOUT,
                AppError::new("request_timeout", "读取请求正文超时。"),
            )
        }
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return respond_error(AppError::invalid("请求正文不是有效 JSON。")),
    };
    if let Err(error) = validate_api_body(kind, &body) {
        return respond_error(error);
    }
    drop(permit);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let inference = tokio::select! {biased;_=state.shutdown.cancelled()=>return respond_error(AppError::new("service_stopped","服务正在停止。")),result=state.engine.infer(kind,body,id.clone(),state.settings.clone())=>result};
    let mut inference = match inference {
        Ok(i) => i,
        Err(e) => return respond_error(e),
    };
    if inference.content_type == "text/event-stream" {
        let stream = async_stream::stream! {
            loop {
                let next=tokio::select!{biased;_=state.shutdown.cancelled()=>Some(Err(AppError::new("cancelled","服务停止，请求已取消。"))),next=inference.stream.next()=>next};
                match next {
                    Some(Ok(bytes))=>yield Ok::<Bytes,Infallible>(bytes),
                    Some(Err(error))=>{yield Ok(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n",error_json(&error))));break;},
                    None=>break,
                }
            }
        };
        let mut response = Body::from_stream(stream).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert("x-accel-buffering", header::HeaderValue::from_static("no"));
        if let Ok(value) = header::HeaderValue::from_str(&id) {
            response.headers_mut().insert("x-request-id", value);
        }
        response
    } else {
        let mut buffer = Vec::new();
        loop {
            let next = tokio::select! {biased;_=state.shutdown.cancelled()=>return respond_error(AppError::new("service_stopped","服务停止，请求已取消。")),next=inference.stream.next()=>next};
            match next {
                Some(Ok(bytes)) => {
                    buffer.extend_from_slice(&bytes);
                    if buffer.len() > 32 * 1024 * 1024 {
                        return respond_error(AppError::new(
                            "response_too_large",
                            "响应超过限制。",
                        ));
                    }
                }
                Some(Err(e)) => return respond_error(e),
                None => break,
            }
        }
        match serde_json::from_slice::<Value>(&buffer) {
            Ok(body) => {
                let mut response = Json(body).into_response();
                if let Ok(value) = header::HeaderValue::from_str(&id) {
                    response.headers_mut().insert("x-request-id", value);
                }
                response
            }
            Err(_) => respond_error(AppError::new("invalid_response", "推理响应格式无效。")),
        }
    }
}
fn error_json(error: &AppError) -> Value {
    json!({"error":{"message":error.message,"type":match error.code.as_str(){"rate_limit_exceeded"=>"rate_limit_error","timeout"|"request_timeout"=>"timeout_error","invalid_request"|"context_limit"|"model_not_found"=>"invalid_request_error",_=>"server_error"},"param":Value::Null,"code":error.code}})
}
fn error_response(status: StatusCode, error: AppError) -> Response {
    (status, Json(error_json(&error))).into_response()
}
fn respond_error(error: AppError) -> Response {
    let status = match error.code.as_str() {
        "invalid_request" | "context_limit" => StatusCode::BAD_REQUEST,
        "model_not_found" => StatusCode::NOT_FOUND,
        "duplicate_request" => StatusCode::CONFLICT,
        "rate_limit_exceeded" | "engine_busy" => StatusCode::TOO_MANY_REQUESTS,
        "timeout" => StatusCode::GATEWAY_TIMEOUT,
        "cancelled" => StatusCode::REQUEST_TIMEOUT,
        "model_not_loaded" | "engine_unavailable" | "service_stopped" => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::BAD_GATEWAY,
    };
    error_response(status, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    fn app() -> Router {
        router(HttpState {
            engine: EngineManager::new(std::path::PathBuf::new()),
            settings: ServiceSettings::default(),
            shutdown: CancellationToken::new(),
            readers: Arc::new(Semaphore::new(2)),
        })
    }
    async fn check(request: Request, expected: StatusCode) {
        let response = app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["error"]["code"].is_string());
        assert!(!String::from_utf8_lossy(&bytes).contains("C:\\"));
    }
    #[tokio::test]
    async fn malformed_missing_unknown_and_method_errors_are_json() {
        check(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from("{bad"))
                .unwrap(),
            StatusCode::BAD_REQUEST,
        )
        .await;
        check(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
            StatusCode::BAD_REQUEST,
        )
        .await;
        check(
            Request::builder()
                .uri("/missing")
                .body(Body::empty())
                .unwrap(),
            StatusCode::NOT_FOUND,
        )
        .await;
        check(
            Request::builder()
                .uri("/v1/embeddings")
                .body(Body::empty())
                .unwrap(),
            StatusCode::METHOD_NOT_ALLOWED,
        )
        .await;
    }
    #[tokio::test]
    async fn body_limits_and_browser_origins_are_rejected() {
        check(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(1024 * 1024 + 1)))
                .unwrap(),
            StatusCode::PAYLOAD_TOO_LARGE,
        )
        .await;
        check(
            Request::builder()
                .uri("/health")
                .header("origin", "https://untrusted.example")
                .body(Body::empty())
                .unwrap(),
            StatusCode::FORBIDDEN,
        )
        .await;
        check(
            Request::builder()
                .uri("/health")
                .header("host", "rebind.example:8848")
                .body(Body::empty())
                .unwrap(),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    #[tokio::test]
    async fn valid_request_without_model_is_explicit() {
        check(
            Request::builder()
                .method("POST")
                .uri("/v1/embeddings")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"qwen","input":"hello"}"#))
                .unwrap(),
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
    }
    #[tokio::test]
    async fn lifecycle_releases_port_and_refuses_duplicate_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let settings = ServiceSettings {
            port,
            ..ServiceSettings::default()
        };
        let engine = EngineManager::new(std::path::PathBuf::new());
        let mut server = start(engine.clone(), settings.clone()).await.unwrap();
        assert!(server.is_running());
        assert!(start(engine.clone(), settings.clone()).await.is_err());
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert!(client
            .get(format!("{}/health", server.address))
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        server.stop().await;
        assert!(!server.is_running());
        let mut restarted = start(engine, settings).await.unwrap();
        restarted.stop().await;
    }
}
