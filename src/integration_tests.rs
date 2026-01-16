use crate::config::{AppConfig, ProviderConfig};
use crate::state::AppState;
use crate::{api, proxy};
use axum::{
    routing::{any, get, post},
    Router,
};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::TcpListener;

fn enabled() -> bool {
    std::env::var("EAVS_INTEGRATION_TESTS").ok().as_deref() == Some("1")
}

fn tool_calls_enabled() -> bool {
    std::env::var("EAVS_INTEGRATION_TOOL_CALLS").ok().as_deref() == Some("1")
}

async fn serve(app: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, handle)
}

fn base_config(providers: HashMap<String, ProviderConfig>) -> AppConfig {
    AppConfig {
        server: Default::default(),
        providers,
        upstream: HashMap::new(),
        logging: Default::default(),
        analysis: Default::default(),
        policy: Default::default(),
        state: Default::default(),
        keys: Default::default(),
        capture: Default::default(),
    }
}

#[tokio::test]
async fn integration_openai_chat() {
    if !enabled() {
        return;
    }
    if std::env::var("OPENAI_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_OPENAI_MODEL")
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "openai".to_string(),
            api_key: "env:OPENAI_API_KEY".to_string(),
            base_url: String::new(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/health", get(api::health_handler))
        .route("/inject/:conversation_id", post(api::inject_handler))
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16
    });
    let resp = client.post(url).json(&body).send().await.unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json.get("choices").is_some());
}

#[tokio::test]
async fn integration_openai_streaming() {
    if !enabled() {
        return;
    }
    if std::env::var("OPENAI_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_OPENAI_MODEL")
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "openai".to_string(),
            api_key: "env:OPENAI_API_KEY".to_string(),
            base_url: String::new(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16,
        "stream": true
    });
    let resp = client.post(url).json(&body).send().await.unwrap();
    assert!(resp.status().is_success());
    let text = resp.text().await.unwrap();
    assert!(text.contains("[DONE]"));
}

#[tokio::test]
async fn integration_anthropic_via_provider_header() {
    if !enabled() {
        return;
    }
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-3-5-sonnet-20240620".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "anthropic".to_string(),
            api_key: "env:ANTHROPIC_API_KEY".to_string(),
            base_url: String::new(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16
    });
    let resp = client
        .post(url)
        .header("X-Provider", "default")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json.get("choices").is_some());
}

#[tokio::test]
async fn integration_anthropic_streaming() {
    if !enabled() {
        return;
    }
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-3-5-sonnet-20240620".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "anthropic".to_string(),
            api_key: "env:ANTHROPIC_API_KEY".to_string(),
            base_url: String::new(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16,
        "stream": true
    });
    let resp = client
        .post(url)
        .header("X-Provider", "default")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let text = resp.text().await.unwrap();
    assert!(text.contains("[DONE]"));
}

#[tokio::test]
async fn integration_ollama_if_available() {
    if !enabled() {
        return;
    }

    let base_url = std::env::var("EAVS_INTEGRATION_OLLAMA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:11434/v1".to_string());
    let model =
        std::env::var("EAVS_INTEGRATION_OLLAMA_MODEL").unwrap_or_else(|_| "llama3".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "ollama".to_string(),
            api_key: String::new(),
            base_url,
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16
    });

    // If Ollama isn't running, just skip.
    let resp = match client.post(url).json(&body).send().await {
        Ok(r) => r,
        Err(_) => return,
    };
    if !resp.status().is_success() {
        return;
    }
    let json: serde_json::Value = resp.json().await.unwrap();
    assert!(json.get("choices").is_some());
}

#[tokio::test]
async fn integration_openai_models_endpoint() {
    if !enabled() {
        return;
    }
    if std::env::var("OPENAI_API_KEY").is_err() {
        return;
    }

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "openai".to_string(),
            api_key: "env:OPENAI_API_KEY".to_string(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/models", addr);
    let resp = client.get(url).send().await.unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn integration_anthropic_models_endpoint() {
    if !enabled() {
        return;
    }
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "anthropic".to_string(),
            api_key: "env:ANTHROPIC_API_KEY".to_string(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/models", addr);
    let resp = client
        .get(url)
        .header("X-Provider", "default")
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
}

#[tokio::test]
async fn integration_mistral_chat_and_streaming() {
    if !enabled() {
        return;
    }
    if std::env::var("MISTRAL_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_MISTRAL_MODEL")
        .unwrap_or_else(|_| "mistral-large-latest".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "mistral".to_string(),
            api_key: "env:MISTRAL_API_KEY".to_string(),
            base_url: std::env::var("EAVS_INTEGRATION_MISTRAL_URL").unwrap_or_default(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);

    // Non-streaming
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16
    });
    let resp = client
        .post(&url)
        .header("X-Provider", "default")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Streaming
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Say 'ok'"}],
        "max_tokens": 16,
        "stream": true
    });
    let resp = client
        .post(&url)
        .header("X-Provider", "default")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let text = resp.text().await.unwrap();
    assert!(text.contains("[DONE]"));
}

#[tokio::test]
async fn integration_tool_calls_mistral_required_translates_to_any() {
    if !enabled() || !tool_calls_enabled() {
        return;
    }
    if std::env::var("MISTRAL_API_KEY").is_err() {
        return;
    }

    let model = std::env::var("EAVS_INTEGRATION_MISTRAL_MODEL")
        .unwrap_or_else(|_| "mistral-large-latest".to_string());

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "mistral".to_string(),
            api_key: "env:MISTRAL_API_KEY".to_string(),
            base_url: std::env::var("EAVS_INTEGRATION_MISTRAL_URL").unwrap_or_default(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/chat/completions", addr);
    let body = json!({
        "model": model,
        "messages": [{"role":"user","content":"Call the noop tool with an empty JSON object. Do not answer normally."}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "noop",
                "description": "noop",
                "parameters": {"type":"object","properties":{}}
            }
        }],
        "tool_choice": "required",
        "max_tokens": 64
    });

    let resp = client
        .post(url)
        .header("X-Provider", "default")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    let tool_calls = json["choices"][0]["message"].get("tool_calls").cloned();
    assert!(tool_calls.is_some());
}

#[tokio::test]
async fn integration_models_endpoint_synthetic_bedrock() {
    if !enabled() {
        return;
    }

    let mut providers = HashMap::new();
    providers.insert(
        "default".to_string(),
        ProviderConfig {
            type_: "bedrock".to_string(),
            ..Default::default()
        },
    );

    let state = AppState::new(base_config(providers));
    let app = Router::new()
        .route("/v1/*path", any(proxy::proxy_handler))
        .with_state(state);
    let (addr, _handle) = serve(app).await;

    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/models", addr);
    let resp = client.get(url).send().await.unwrap();
    assert!(resp.status().is_success());
    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["object"], "list");
    assert!(json["data"].is_array());
}
