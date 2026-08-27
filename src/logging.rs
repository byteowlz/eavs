//! Logging backend system for EAVS.
//!
//! Supports multiple logging backends:
//! - Stdout: JSON or pretty-printed logs
//! - File: Append logs to file with optional rotation
//! - Webhook: POST logs to HTTP endpoint
//! - OpenTelemetry: OTLP export (future)

use crate::config::{LogBackend, LoggingConfig};
use crate::state::AnalysisEvent;
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

/// Trait for logging backends.
pub trait LogSink: Send + Sync {
    /// Log an analysis event.
    fn log(&self, event: &AnalysisEvent);

    /// Flush any buffered logs.
    fn flush(&self) {}

    /// Get the backend name for debugging.
    fn name(&self) -> &'static str;
}

/// Stdout logging backend.
pub struct StdoutSink {
    pretty: bool,
}

impl StdoutSink {
    pub fn new(format: &str) -> Self {
        Self {
            pretty: format == "pretty",
        }
    }
}

impl LogSink for StdoutSink {
    fn log(&self, event: &AnalysisEvent) {
        if self.pretty {
            match event {
                AnalysisEvent::Request {
                    timestamp,
                    id,
                    method,
                    uri,
                    body,
                } => {
                    let time = format_timestamp(*timestamp);
                    println!("[{time}] {id} {method} {uri}");
                    if let Some(model) = body.get("model").and_then(|v| v.as_str()) {
                        println!("  model: {model}");
                    }
                }
                AnalysisEvent::ResponseChunk {
                    timestamp: _,
                    id: _,
                    chunk,
                } => {
                    // For pretty mode, just print raw chunks (SSE data)
                    print!("{chunk}");
                }
                AnalysisEvent::DelegatedFetchStripped {
                    timestamp,
                    id,
                    field_path,
                    capability,
                    target_host,
                } => {
                    let time = format_timestamp(*timestamp);
                    let host = target_host.as_deref().unwrap_or("-");
                    eprintln!(
                        "[{time}] {id} SECURITY delegated-fetch stripped: capability={capability} path={field_path} host={host}"
                    );
                }
                AnalysisEvent::ResponseComplete {
                    timestamp,
                    id,
                    status,
                    duration_ms,
                } => {
                    let time = format_timestamp(*timestamp);
                    println!("\n[{time}] {id} completed: status={status} duration={duration_ms}ms");
                }
                AnalysisEvent::Error {
                    timestamp,
                    id,
                    error,
                } => {
                    let time = format_timestamp(*timestamp);
                    eprintln!("[{time}] {id} ERROR: {error}");
                }
            }
        } else {
            // JSON output
            if let Ok(json) = serde_json::to_string(event) {
                println!("{json}");
            }
        }
    }

    fn name(&self) -> &'static str {
        "stdout"
    }
}

/// File logging backend.
pub struct FileSink {
    #[allow(dead_code)]
    path: String,
    file: Arc<RwLock<Option<File>>>,
}

impl FileSink {
    pub fn new(path: &str, _rotate: &str) -> Self {
        let file = Self::open_file(path);
        Self {
            path: path.to_string(),
            file: Arc::new(RwLock::new(file)),
        }
    }

    fn open_file(path: &str) -> Option<File> {
        // Create parent directories if needed
        if let Some(parent) = Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        OpenOptions::new().create(true).append(true).open(path).ok()
    }
}

impl LogSink for FileSink {
    fn log(&self, event: &AnalysisEvent) {
        if let Ok(json) = serde_json::to_string(event) {
            // Use blocking write for simplicity
            if let Ok(mut guard) = self.file.try_write() {
                if let Some(ref mut file) = *guard {
                    let _ = writeln!(file, "{json}");
                }
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.try_write() {
            if let Some(ref mut file) = *guard {
                let _ = file.flush();
            }
        }
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

/// Webhook logging backend with batching.
pub struct WebhookSink {
    url: String,
    headers: Vec<(String, String)>,
    batch_size: usize,
    flush_interval_secs: u64,
    buffer: Arc<RwLock<Vec<AnalysisEvent>>>,
    client: reqwest::Client,
}

impl WebhookSink {
    pub fn new(
        url: &str,
        headers: &std::collections::HashMap<String, String>,
        batch_size: usize,
        flush_interval_secs: u64,
    ) -> Self {
        let resolved_headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| {
                let value = if let Some(var_name) = v.strip_prefix("env:") {
                    std::env::var(var_name).unwrap_or_default()
                } else {
                    v.clone()
                };
                (k.clone(), value)
            })
            .collect();

        Self {
            url: url.to_string(),
            headers: resolved_headers,
            batch_size,
            flush_interval_secs,
            buffer: Arc::new(RwLock::new(Vec::with_capacity(batch_size))),
            client: reqwest::Client::new(),
        }
    }

    /// Start the background flush task.
    pub fn start_flush_task(&self) -> mpsc::Sender<()> {
        let (tx, mut rx) = mpsc::channel::<()>(1);
        let url = self.url.clone();
        let headers = self.headers.clone();
        let buffer = self.buffer.clone();
        let client = self.client.clone();
        let interval = self.flush_interval_secs;

        tokio::spawn(async move {
            let mut interval_timer =
                tokio::time::interval(tokio::time::Duration::from_secs(interval));
            loop {
                tokio::select! {
                    _ = interval_timer.tick() => {
                        Self::flush_buffer(&url, &headers, &buffer, &client).await;
                    }
                    _ = rx.recv() => {
                        // Shutdown signal
                        Self::flush_buffer(&url, &headers, &buffer, &client).await;
                        break;
                    }
                }
            }
        });

        tx
    }

    async fn flush_buffer(
        url: &str,
        headers: &[(String, String)],
        buffer: &RwLock<Vec<AnalysisEvent>>,
        client: &reqwest::Client,
    ) {
        let events: Vec<AnalysisEvent> = {
            let mut guard = buffer.write().await;
            std::mem::take(&mut *guard)
        };

        if events.is_empty() {
            return;
        }

        let mut req = client.post(url).json(&events);
        for (key, value) in headers {
            req = req.header(key, value);
        }

        if let Err(e) = req.send().await {
            tracing::warn!("Failed to send logs to webhook: {}", e);
        }
    }
}

impl LogSink for WebhookSink {
    fn log(&self, event: &AnalysisEvent) {
        if let Ok(mut guard) = self.buffer.try_write() {
            guard.push(event.clone());

            // Trigger immediate flush if batch is full
            if guard.len() >= self.batch_size {
                let events = std::mem::take(&mut *guard);
                let url = self.url.clone();
                let headers = self.headers.clone();
                let client = self.client.clone();

                tokio::spawn(async move {
                    let mut req = client.post(&url).json(&events);
                    for (key, value) in &headers {
                        req = req.header(key, value);
                    }
                    let _ = req.send().await;
                });
            }
        }
    }

    fn name(&self) -> &'static str {
        "webhook"
    }
}

/// Null sink for disabled logging.
#[allow(dead_code)]
pub struct NullSink;

impl LogSink for NullSink {
    fn log(&self, _event: &AnalysisEvent) {}

    fn name(&self) -> &'static str {
        "null"
    }
}

/// Logger manager that dispatches to multiple backends.
pub struct Logger {
    sinks: Vec<Box<dyn LogSink>>,
    /// When false (default), request/response *content* is stripped before any
    /// sink sees it -- only metadata is logged. See `LoggingConfig::log_bodies`.
    log_bodies: bool,
}

/// Strip a request body to non-content routing metadata, dropping the
/// `messages`/prompt content entirely.
fn redact_request_body(body: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for key in ["model", "stream", "max_tokens", "temperature", "top_p"] {
        if let Some(v) = body.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    out.insert(
        "_content_redacted".to_string(),
        serde_json::Value::Bool(true),
    );
    serde_json::Value::Object(out)
}

impl Logger {
    /// Create a new logger from configuration.
    pub fn from_config(config: &LoggingConfig) -> Self {
        let mut sinks: Vec<Box<dyn LogSink>> = Vec::new();

        // Handle default sink
        let default_sink = config.effective_default();
        match default_sink {
            "stdout" => sinks.push(Box::new(StdoutSink::new("json"))),
            "pretty" => sinks.push(Box::new(StdoutSink::new("pretty"))),
            "none" | "null" => {}
            _ => sinks.push(Box::new(StdoutSink::new("json"))),
        }

        // Handle explicit backends
        for backend in &config.backends {
            match backend {
                LogBackend::Stdout { format } => {
                    sinks.push(Box::new(StdoutSink::new(format)));
                }
                LogBackend::File { path, rotate, .. } => {
                    sinks.push(Box::new(FileSink::new(path, rotate)));
                }
                LogBackend::Webhook {
                    url,
                    headers,
                    batch_size,
                    flush_interval_secs,
                } => {
                    let sink = WebhookSink::new(url, headers, *batch_size, *flush_interval_secs);
                    sink.start_flush_task();
                    sinks.push(Box::new(sink));
                }
                LogBackend::OpenTelemetry {
                    endpoint,
                    protocol,
                    service_name,
                } => {
                    // OTEL support is a future enhancement
                    tracing::info!(
                        "OpenTelemetry logging configured (endpoint={}, protocol={}, service={}), but not yet implemented",
                        endpoint, protocol, service_name
                    );
                }
            }
        }

        // Default to stdout if no sinks configured
        if sinks.is_empty() {
            sinks.push(Box::new(StdoutSink::new("json")));
        }

        Self {
            sinks,
            log_bodies: config.log_bodies,
        }
    }

    /// Log an event to all backends.
    ///
    /// When `log_bodies` is disabled (the default), conversation *content* never
    /// reaches a sink: response chunks (pure model output) are dropped and
    /// request bodies are stripped to routing metadata.
    pub fn log(&self, event: &AnalysisEvent) {
        if self.log_bodies {
            for sink in &self.sinks {
                sink.log(event);
            }
            return;
        }
        match event {
            // Pure model output -- never logged without explicit opt-in.
            AnalysisEvent::ResponseChunk { .. } => {}
            AnalysisEvent::Request {
                timestamp,
                id,
                method,
                uri,
                body,
            } => {
                let redacted = AnalysisEvent::Request {
                    timestamp: *timestamp,
                    id: id.clone(),
                    method: method.clone(),
                    uri: uri.clone(),
                    body: redact_request_body(body),
                };
                for sink in &self.sinks {
                    sink.log(&redacted);
                }
            }
            // Metadata-only events (ResponseComplete, Error) pass through.
            other => {
                for sink in &self.sinks {
                    sink.log(other);
                }
            }
        }
    }

    /// Flush all backends.
    pub fn flush(&self) {
        for sink in &self.sinks {
            sink.flush();
        }
    }

    /// Get the names of configured sinks.
    pub fn sink_names(&self) -> Vec<&'static str> {
        self.sinks.iter().map(|s| s.name()).collect()
    }
}

/// Start a logging task that consumes from a broadcast channel.
pub fn start_logging_task(
    logger: Arc<Logger>,
    mut rx: broadcast::Receiver<AnalysisEvent>,
) -> mpsc::Sender<()> {
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(event) => logger.log(&event),
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Logger lagged, missed {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            break;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    logger.flush();
                    break;
                }
            }
        }
    });

    shutdown_tx
}

fn format_timestamp(ts: i64) -> String {
    use chrono::TimeZone;
    Utc.timestamp_millis_opt(ts)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_stdout_sink_json() {
        let sink = StdoutSink::new("json");
        assert_eq!(sink.name(), "stdout");

        let event = AnalysisEvent::Request {
            timestamp: 1234567890000,
            id: "test-id".to_string(),
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            body: serde_json::json!({"model": "gpt-4"}),
        };

        // Just verify it doesn't panic
        sink.log(&event);
    }

    #[test]
    fn test_null_sink() {
        let sink = NullSink;
        assert_eq!(sink.name(), "null");

        let event = AnalysisEvent::Request {
            timestamp: 1234567890000,
            id: "test-id".to_string(),
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            body: serde_json::json!({}),
        };

        sink.log(&event); // Should do nothing
    }

    #[test]
    fn test_logger_from_default_config() {
        let config = LoggingConfig::default();
        let logger = Logger::from_config(&config);

        assert!(!logger.sinks.is_empty());
        assert!(logger.sink_names().contains(&"stdout"));
    }

    #[test]
    fn test_logger_with_none() {
        let config = LoggingConfig {
            default: "none".to_string(),
            sink: String::new(),
            backends: Vec::new(),
            log_bodies: false,
        };
        let logger = Logger::from_config(&config);

        // Should still have stdout as fallback since backends is empty
        assert!(logger.sink_names().contains(&"stdout"));
    }

    /// Sink that records every event (serialized) for assertions.
    struct CaptureSink(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl LogSink for CaptureSink {
        fn log(&self, event: &AnalysisEvent) {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::to_string(event).unwrap());
        }
        fn name(&self) -> &'static str {
            "capture"
        }
    }

    fn request_with_secret() -> AnalysisEvent {
        AnalysisEvent::Request {
            timestamp: 0,
            id: "x".into(),
            method: "POST".into(),
            uri: "/v1/chat".into(),
            body: serde_json::json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "SECRET PROMPT"}]
            }),
        }
    }

    #[test]
    fn redacts_content_when_log_bodies_disabled() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = Logger {
            sinks: vec![Box::new(CaptureSink(captured.clone()))],
            log_bodies: false,
        };
        logger.log(&request_with_secret());
        logger.log(&AnalysisEvent::ResponseChunk {
            timestamp: 0,
            id: "x".into(),
            chunk: "SECRET OUTPUT".into(),
        });

        let logs = captured.lock().unwrap();
        let joined = logs.join("\n");
        assert!(
            !joined.contains("SECRET PROMPT"),
            "prompt must be redacted: {joined}"
        );
        assert!(
            !joined.contains("SECRET OUTPUT"),
            "model output must not log: {joined}"
        );
        assert!(
            joined.contains("gpt-4"),
            "model metadata should remain: {joined}"
        );
        assert!(joined.contains("_content_redacted"));
        // The response chunk (pure content) is dropped -> only the request log.
        assert_eq!(logs.len(), 1);
    }

    #[test]
    fn logs_content_when_opted_in() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let logger = Logger {
            sinks: vec![Box::new(CaptureSink(captured.clone()))],
            log_bodies: true,
        };
        logger.log(&request_with_secret());
        assert!(captured
            .lock()
            .unwrap()
            .join("\n")
            .contains("SECRET PROMPT"));
    }

    #[test]
    fn test_webhook_header_env_resolution() {
        std::env::set_var("TEST_WEBHOOK_KEY", "secret123");

        let mut headers = HashMap::new();
        headers.insert(
            "Authorization".to_string(),
            "env:TEST_WEBHOOK_KEY".to_string(),
        );
        headers.insert("X-Custom".to_string(), "static-value".to_string());

        let sink = WebhookSink::new("https://example.com/logs", &headers, 10, 5);

        // Check that env var was resolved
        let auth_header = sink.headers.iter().find(|(k, _)| k == "Authorization");
        assert!(auth_header.is_some());
        assert_eq!(auth_header.unwrap().1, "secret123");

        let custom_header = sink.headers.iter().find(|(k, _)| k == "X-Custom");
        assert!(custom_header.is_some());
        assert_eq!(custom_header.unwrap().1, "static-value");

        std::env::remove_var("TEST_WEBHOOK_KEY");
    }

    #[test]
    fn test_format_timestamp() {
        let ts = 1234567890000i64; // 2009-02-13 23:31:30 UTC
        let formatted = format_timestamp(ts);
        assert!(formatted.contains("2009"));
        assert!(formatted.contains("23:31:30"));
    }
}
