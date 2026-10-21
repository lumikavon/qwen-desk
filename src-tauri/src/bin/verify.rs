//! Reproducible verification using actual downloaded model weights, never synthetic inference.
use futures_util::StreamExt;
use qwen_desk::{
    config::{ModelKind, Paths, ServiceSettings},
    error::{AppError, Result},
    AppCore,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("FAIL {}: {}", e.code, e.message);
        std::process::exit(1);
    }
}
async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("smoke");
    let data = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
            .join("com.qwendesk.desktop")
    });
    let runtime = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime");
    let core = AppCore::new(Paths::new(data, runtime)).await?;
    let result = match mode {
        "download" => download(&core).await,
        "smoke" => smoke(&core).await,
        "embedding" => embedding_smoke(&core).await,
        "resilience" => resilience(&core).await,
        "serve" => serve(&core).await,
        _ => Err(AppError::invalid(
            "Usage: qwen-verify [download|smoke|serve] [data-directory]",
        )),
    };
    core.shutdown().await;
    result
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        println!("PASS {message}");
        Ok(())
    } else {
        Err(AppError::new("verification_failed", message))
    }
}
async fn download(core: &Arc<AppCore>) -> Result<()> {
    let status = core.downloads.credential_status().await;
    println!("Credential status: {}", status);
    for id in ["qwen3-generation", "qwen3-embedding"] {
        let model = core.model(id).await?;
        if model.resolved_path(&core.paths).is_file() {
            println!("Already present: {id}");
            continue;
        }
        core.download(id).await?;
    }
    let deadline = Instant::now() + Duration::from_secs(14400);
    let mut cancelled = false;
    loop {
        let snapshot = core.downloads.snapshot().await;
        println!("Download progress: {snapshot}");
        let records = snapshot
            .as_array()
            .ok_or_else(|| AppError::new("verification_failed", "Invalid download snapshot"))?;
        if !cancelled
            && records.iter().any(|r| {
                r["id"] == "qwen3-generation"
                    && r["downloaded"].as_u64().unwrap_or(0) > 2 * 1024 * 1024
                    && r["status"] == "downloading"
            })
        {
            core.downloads.cancel("qwen3-generation").await;
            println!("PASS requested cancellation of actual model download");
            cancelled = true;
        }
        if cancelled
            && records
                .iter()
                .any(|r| r["id"] == "qwen3-generation" && r["status"] == "cancelled")
        {
            core.download("qwen3-generation").await?;
            println!("PASS resumed cancelled model download");
        }
        if let Some(r) = records.iter().find(|r| r["status"] == "error") {
            return Err(AppError::new("download_failed", r["error"].to_string()));
        }
        let mut complete = true;
        for id in ["qwen3-generation", "qwen3-embedding"] {
            complete &= core.model(id).await?.resolved_path(&core.paths).is_file();
        }
        if complete {
            return require(
                true,
                "both default GGUF model files downloaded and verified",
            );
        }
        if Instant::now() > deadline {
            return Err(AppError::new(
                "timeout",
                "Download verification exceeded four hours",
            ));
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
fn chat(name: &str, stream: bool) -> Value {
    json!({"model":name,"messages":[{"role":"user","content":"Translate into English: 你好，世界。 Reply with the translation only."}],"temperature":0.0,"top_p":1.0,"max_tokens":48,"stream":stream})
}
async fn collect(core: &Arc<AppCore>, kind: ModelKind, body: Value, id: &str) -> Result<Vec<u8>> {
    let limits = core.settings.read().await.service.clone();
    let mut inference = core.engine.infer(kind, body, id.into(), limits).await?;
    let mut bytes = vec![];
    while let Some(chunk) = inference.stream.next().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok(bytes)
}
async fn smoke(core: &Arc<AppCore>) -> Result<()> {
    let gen = core.model("qwen3-generation").await?;
    let emb = core.model("qwen3-embedding").await?;
    core.load(&gen.id).await?;
    core.load(&emb.id).await?;
    println!("Loaded: {}", core.engine.snapshot().await);
    let body = collect(
        core,
        ModelKind::Generation,
        chat(&gen.name, false),
        "verify-json",
    )
    .await?;
    let response: Value = serde_json::from_slice(&body)
        .map_err(|_| AppError::new("verification_failed", "Invalid chat JSON"))?;
    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("");
    println!("Actual translation: {content}");
    require(
        content.to_lowercase().contains("hello")
            && response["usage"]["completion_tokens"].as_u64().unwrap_or(0) > 0,
        "actual default generation model translates Chinese to English",
    )?;
    let vector:Value=serde_json::from_slice(&collect(core,ModelKind::Embedding,json!({"model":emb.name,"input":["本地推理","local inference"],"encoding_format":"float"}),"verify-embedding").await?).map_err(|_|AppError::new("verification_failed","Invalid embedding JSON"))?;
    let vectors = vector["data"]
        .as_array()
        .ok_or_else(|| AppError::new("verification_failed", "Missing vectors"))?;
    require(vectors.len() == 2, "embedding batch has two results")?;
    for v in vectors {
        let values = v["embedding"]
            .as_array()
            .ok_or_else(|| AppError::new("verification_failed", "Missing embedding array"))?;
        let norm = values
            .iter()
            .map(|x| x.as_f64().unwrap_or(f64::NAN).powi(2))
            .sum::<f64>()
            .sqrt();
        require(
            values.len() == 1024 && norm.is_finite() && (norm - 1.0).abs() < 0.01,
            "actual embedding is 1024 finite values with unit norm",
        )?;
    }
    require(
        vector["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0,
        "embedding usage contains actual tokens",
    )?;
    let stream = String::from_utf8(
        collect(
            core,
            ModelKind::Generation,
            chat(&gen.name, true),
            "verify-stream",
        )
        .await?,
    )
    .map_err(|_| AppError::new("verification_failed", "Invalid UTF8 stream"))?;
    require(
        stream.contains("data: [DONE]")
            && stream.contains("chat.completion.chunk")
            && stream.contains("finish_reason"),
        "real streaming chat includes chunks, finish reason and DONE",
    )?;
    let settings = ServiceSettings {
        host: "127.0.0.1".into(),
        port: 18848,
        concurrency: 1,
        timeout_secs: 60,
        auto_start: false,
    };
    let mut server = qwen_desk::api::start(core.engine.clone(), settings.clone()).await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|_| AppError::new("verification_failed", "HTTP client failed"))?;
    let base = "http://127.0.0.1:18848";
    let result = http_checks(&client, base, &gen.name, &emb.name).await;
    require(
        qwen_desk::api::start(core.engine.clone(), settings)
            .await
            .is_err(),
        "occupied port returns error",
    )?;
    server.stop().await;
    result?;
    require(
        client.get(format!("{base}/health")).send().await.is_err(),
        "stopped service releases listener",
    )?;
    let mut server = qwen_desk::api::start(
        core.engine.clone(),
        ServiceSettings {
            port: 18849,
            ..ServiceSettings::default()
        },
    )
    .await?;
    require(
        client
            .get("http://127.0.0.1:18849/health")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        "service restarts on changed port",
    )?;
    server.stop().await;
    let mut long = chat(&gen.name, true);
    long["max_tokens"] = json!(2048);
    long["messages"][0]["content"] =
        json!("Write a very long numbered list of 1000 creative story ideas.");
    let mut active = core
        .engine
        .infer(
            ModelKind::Generation,
            long,
            "verify-cancel".into(),
            ServiceSettings::default(),
        )
        .await?;
    require(
        active.stream.next().await.transpose()?.is_some(),
        "cancellation test starts actual inference",
    )?;
    core.engine.cancel("verify-cancel").await;
    let mut observed = false;
    while let Some(next) = active.stream.next().await {
        if let Err(e) = next {
            observed = e.code.contains("cancel");
            break;
        }
    }
    require(observed, "in-progress generation observes cancellation")?;
    drop(active);
    core.engine.unload(ModelKind::Generation).await?;
    core.engine.unload(ModelKind::Embedding).await?;
    require(
        core.engine
            .infer(
                ModelKind::Generation,
                chat(&gen.name, false),
                "unloaded-test".into(),
                ServiceSettings::default(),
            )
            .await
            .is_err(),
        "unloaded model refuses inference",
    )?;
    // Explicit CPU execution, reload and repeated unload verify resource lifecycle.
    let mut settings = core.settings.read().await.clone();
    settings.inference.gpu_layers = 0;
    *core.settings.write().await = settings;
    core.load(&emb.id).await?;
    let cpu = collect(
        core,
        ModelKind::Embedding,
        json!({"model":emb.name,"input":"CPU fallback verification"}),
        "verify-cpu",
    )
    .await?;
    require(
        serde_json::from_slice::<Value>(&cpu)
            .ok()
            .is_some_and(|v| v["data"][0]["embedding"].is_array()),
        "CPU backend produces actual embedding",
    )?;
    println!("CPU snapshot: {}", core.engine.snapshot().await);
    core.engine.unload(ModelKind::Embedding).await?;
    core.engine.unload(ModelKind::Embedding).await?;
    require(true, "repeat unload is safe")?;
    Ok(())
}
async fn http_checks(client: &reqwest::Client, base: &str, gen: &str, emb: &str) -> Result<()> {
    for (route, body, field) in [
        ("chat/completions", chat(gen, false), "choices"),
        (
            "embeddings",
            json!({"model":emb,"input":"HTTP embedding verification"}),
            "data",
        ),
    ] {
        let response = client
            .post(format!("{base}/v1/{route}"))
            .json(&body)
            .send()
            .await
            .map_err(|_| AppError::new("verification_failed", "HTTP inference failed"))?;
        require(
            response.status().is_success(),
            &format!("HTTP {route} status"),
        )?;
        let body: Value = response
            .json()
            .await
            .map_err(|_| AppError::new("verification_failed", "HTTP response not JSON"))?;
        require(
            body[field].is_array(),
            &format!("HTTP {route} response contract"),
        )?;
    }
    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&chat(gen, true))
        .send()
        .await
        .map_err(|_| AppError::new("verification_failed", "HTTP SSE failed"))?;
    require(
        response
            .headers()
            .get("content-type")
            .is_some_and(|v| v.to_str().unwrap_or("").contains("text/event-stream")),
        "HTTP streaming content type",
    )?;
    require(
        response
            .text()
            .await
            .unwrap_or_default()
            .contains("data: [DONE]"),
        "HTTP streaming terminates with DONE",
    )?;
    for body in [
        "{broken",
        r#"{"messages":[]}"#,
        r#"{"model":"missing","messages":[{"role":"user","content":"test"}]}"#,
    ] {
        let response = client
            .post(format!("{base}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .map_err(|_| AppError::new("verification_failed", "Error check HTTP failed"))?;
        require(
            response.status().is_client_error(),
            "invalid request has client error status",
        )?;
        let text = response.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| AppError::new("verification_failed", "Error not JSON"))?;
        require(
            value["error"]["message"].is_string()
                && !text.contains("C:\\")
                && !text.contains("D:\\")
                && !text.contains("hf_"),
            "structured HTTP errors do not disclose local paths or token",
        )?;
    }
    Ok(())
}
async fn wait_idle(core: &Arc<AppCore>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = core.engine.snapshot().await;
        if s["generation"]["active_requests"] == 0 && s["embedding"]["active_requests"] == 0 {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(AppError::new(
                "verification_failed",
                "Request resources were not released",
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
async fn resilience(core: &Arc<AppCore>) -> Result<()> {
    let gen = core.model("qwen3-generation").await?;
    let emb = core.model("qwen3-embedding").await?;
    core.load(&gen.id).await?;
    core.load(&emb.id).await?;
    let mut long = chat(&gen.name, true);
    long["max_tokens"] = json!(4096);
    long["messages"][0]["content"]=json!("Write 1000 detailed numbered explanations of scientific concepts. Continue until all 1000 are complete. Do not summarize or stop early.");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| AppError::io())?;
    let limits = ServiceSettings {
        port: 18850,
        concurrency: 1,
        timeout_secs: 5,
        ..ServiceSettings::default()
    };
    let mut server = qwen_desk::api::start(core.engine.clone(), limits.clone()).await?;
    let mut active = client
        .post("http://127.0.0.1:18850/v1/chat/completions")
        .json(&long)
        .send()
        .await
        .map_err(|_| AppError::io())?;
    require(
        active.chunk().await.map_err(|_| AppError::io())?.is_some(),
        "HTTP concurrency test begins a real stream",
    )?;
    let second = client
        .post("http://127.0.0.1:18850/v1/embeddings")
        .json(&json!({"model":emb.name,"input":"concurrency"}))
        .send()
        .await
        .map_err(|_| AppError::io())?;
    require(
        second.status().as_u16() == 429,
        "shared concurrency limit rejects excess HTTP request with 429",
    )?;
    drop(active);
    wait_idle(core).await?;
    require(true, "client disconnect releases active request")?;
    // A real single-thread CPU workload makes the 5-second deadline deterministic;
    // GPU output may legitimately reach EOS before the timeout.
    let original_params = core.settings.read().await.inference.clone();
    core.engine.unload(ModelKind::Generation).await?;
    {
        let mut settings = core.settings.write().await;
        settings.inference.gpu_layers = 0;
        settings.inference.threads = 1;
    }
    core.load(&gen.id).await?;
    let start = Instant::now();
    let text = client
        .post("http://127.0.0.1:18850/v1/chat/completions")
        .json(&long)
        .send()
        .await
        .map_err(|_| AppError::io())?
        .text()
        .await
        .map_err(|_| AppError::io())?;
    println!(
        "Timeout diagnostic: elapsed_ms={}, DONE={}, tail={}",
        start.elapsed().as_millis(),
        text.contains("[DONE]"),
        text.chars()
            .rev()
            .take(600)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>()
    );
    require(
        text.contains("\"code\":\"timeout\"")
            && text.contains("[DONE]")
            && start.elapsed() < Duration::from_secs(10),
        "stream timeout emits structured SSE error and DONE within configured deadline",
    )?;
    wait_idle(core).await?;
    let mut nonstream = long.clone();
    nonstream["stream"] = json!(false);
    let response = client
        .post("http://127.0.0.1:18850/v1/chat/completions")
        .json(&nonstream)
        .send()
        .await
        .map_err(|_| AppError::io())?;
    require(
        response.status().as_u16() == 504,
        "non-stream timeout returns HTTP 504",
    )?;
    core.settings.write().await.inference = original_params;
    let short=client.post("http://127.0.0.1:18850/v1/embeddings").json(&json!({"model":emb.name,"input":"after timeout","dimensions":128,"encoding_format":"base64"})).send().await.map_err(|_|AppError::io())?.json::<Value>().await.map_err(|_|AppError::io())?;
    use base64::Engine;
    let encoded = short["data"][0]["embedding"].as_str().unwrap_or("");
    require(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .is_ok_and(|b| b.len() == 128 * 4),
        "recovery request returns 128-dimensional base64 embedding",
    )?;
    let mut active = client
        .post("http://127.0.0.1:18850/v1/chat/completions")
        .json(&long)
        .send()
        .await
        .map_err(|_| AppError::io())?;
    require(
        active.chunk().await.map_err(|_| AppError::io())?.is_some(),
        "service-stop test begins real stream",
    )?;
    server.stop().await;
    drop(active);
    wait_idle(core).await?;
    require(
        client
            .get("http://127.0.0.1:18850/health")
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err(),
        "stopping service during inference releases listener and requests",
    )?;
    let mut inference = core
        .engine
        .infer(
            ModelKind::Generation,
            long,
            "unload-active".into(),
            ServiceSettings::default(),
        )
        .await?;
    require(
        inference.stream.next().await.transpose()?.is_some(),
        "unload test starts actual model computation",
    )?;
    core.engine.unload(ModelKind::Generation).await?;
    require(
        inference
            .stream
            .next()
            .await
            .is_some_and(|c| c.is_err_and(|e| e.code == "cancelled")),
        "unloading model cancels its running request",
    )?;
    drop(inference);
    wait_idle(core).await?;
    let loading_core = core.clone();
    let loading_id = gen.id.clone();
    let loading = tokio::spawn(async move { loading_core.load(&loading_id).await });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed = false;
    while Instant::now() < deadline {
        if core.engine.snapshot().await["generation"]["status"] == "loading" {
            observed = true;
            break;
        }
        if loading.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    require(
        observed,
        "loading state is observable without blocking UI polling",
    )?;
    core.engine.unload(ModelKind::Generation).await?;
    require(
        loading
            .await
            .map_err(|_| AppError::io())?
            .is_err_and(|e| e.code == "cancelled"),
        "model loading can be cancelled and reaped",
    )?;
    core.engine.unload(ModelKind::Embedding).await?;
    let mut alternate = emb.clone();
    alternate.id = "verification-alternate".into();
    alternate.name = "alternate-embedding".into();
    alternate.path = emb
        .resolved_path(&core.paths)
        .to_string_lossy()
        .into_owned();
    alternate.source_repo.clear();
    alternate.filename.clear();
    for _ in 0..3 {
        core.engine
            .load(
                alternate.clone(),
                core.settings.read().await.inference.clone(),
            )
            .await?;
        let output = collect(
            core,
            ModelKind::Embedding,
            json!({"model":alternate.name,"input":"alternate model registration"}),
            "alternate-model",
        )
        .await?;
        require(
            serde_json::from_slice::<Value>(&output)
                .ok()
                .is_some_and(|v| v["model"] == alternate.name),
            "replacement embedding model alias produces actual vectors",
        )?;
        core.engine.unload(ModelKind::Embedding).await?;
    }
    let temp = std::env::temp_dir().join(format!("qwen-verify-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(temp.join("cpu"))
        .await
        .map_err(|_| AppError::io())?;
    let outcome = async {
        let mut corrupt = gen.clone();
        corrupt.path = temp.join("corrupt.gguf").to_string_lossy().into_owned();
        tokio::fs::write(
            &corrupt.path,
            b"GGUF\x03\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
        )
        .await
        .map_err(|_| AppError::io())?;
        let error = core
            .engine
            .load(corrupt, core.settings.read().await.inference.clone())
            .await
            .expect_err("corrupt weights must not load");
        require(
            error.code == "model_load_failed"
                && !error.message.contains(&temp.to_string_lossy().to_string()),
            "corrupt GGUF fails safely after native parsing",
        )?;
        let mut files = tokio::fs::read_dir(core.paths.runtime.join("cpu"))
            .await
            .map_err(|_| AppError::io())?;
        while let Some(file) = files.next_entry().await.map_err(|_| AppError::io())? {
            if file
                .file_type()
                .await
                .map_err(|_| AppError::io())?
                .is_file()
            {
                tokio::fs::copy(file.path(), temp.join("cpu").join(file.file_name()))
                    .await
                    .map_err(|_| AppError::io())?;
            }
        }
        let fallback = qwen_desk::engine::EngineManager::new(temp.clone());
        fallback.load(alternate.clone(), Default::default()).await?;
        let status = fallback.snapshot().await;
        require(
            status["embedding"]["mode"] == "CPU"
                && status["embedding"]["error"]["code"] == "cpu_fallback",
            "missing GPU runtime automatically falls back to CPU with visible explanation",
        )?;
        let mut result = fallback
            .infer(
                ModelKind::Embedding,
                json!({"model":alternate.name,"input":"automatic CPU fallback"}),
                "fallback-test".into(),
                Default::default(),
            )
            .await?;
        let mut bytes = vec![];
        while let Some(chunk) = result.stream.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        require(
            serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|v| {
                    v["data"][0]["embedding"]
                        .as_array()
                        .is_some_and(|a| a.len() == 1024)
                }),
            "automatic fallback returns real embeddings",
        )?;
        fallback.shutdown().await;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(temp).await;
    outcome
}
async fn embedding_smoke(core: &Arc<AppCore>) -> Result<()> {
    let model = core.model("qwen3-embedding").await?;
    for cpu in [false, true] {
        core.settings.write().await.inference.gpu_layers = if cpu { 0 } else { -1 };
        core.load(&model.id).await?;
        println!(
            "Loaded embedding mode: {}",
            core.engine.snapshot().await["embedding"]["mode"]
        );
        let value: Value = serde_json::from_slice(
            &collect(
                core,
                ModelKind::Embedding,
                json!({"model":model.name,"input":["本地推理","local inference"]}),
                if cpu {
                    "cpu-embedding"
                } else {
                    "gpu-embedding"
                },
            )
            .await?,
        )
        .map_err(|_| AppError::new("verification_failed", "Invalid embedding JSON"))?;
        let data = value["data"]
            .as_array()
            .ok_or_else(|| AppError::new("verification_failed", "Missing vectors"))?;
        require(data.len() == 2, "embedding batch returns both inputs")?;
        for item in data {
            let vector = item["embedding"]
                .as_array()
                .ok_or_else(|| AppError::new("verification_failed", "Missing vector"))?;
            let norm = vector
                .iter()
                .map(|v| v.as_f64().unwrap_or(f64::NAN).powi(2))
                .sum::<f64>()
                .sqrt();
            require(
                vector.len() == 1024 && (norm - 1.).abs() < 1e-5,
                "real embedding has 1024 finite normalized values",
            )?;
        }
        require(
            value["usage"]["prompt_tokens"].as_u64().unwrap_or(0) > 0,
            "embedding reports tokenizer usage",
        )?;
        core.engine.unload(ModelKind::Embedding).await?;
    }
    Ok(())
}
async fn serve(core: &Arc<AppCore>) -> Result<()> {
    core.load("qwen3-generation").await?;
    core.load("qwen3-embedding").await?;
    core.service_action("start").await?;
    println!("Serving actual models; press Ctrl+C to shut down.");
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| AppError::new("signal_error", "Signal handler failed"))?;
    Ok(())
}
