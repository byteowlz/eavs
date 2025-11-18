use crate::config::AppConfig;
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub client: Client,
    // Key: conversation_id
    pub injections: Arc<DashMap<String, Vec<Injection>>>,
    // Broadcast channel for analysis logs
    pub analysis_tx: broadcast::Sender<AnalysisEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Injection {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InjectionPayload {
    pub messages: Vec<Injection>,
}

#[derive(Debug, Clone, Serialize)]
pub enum AnalysisEvent {
    Request {
        timestamp: i64,
        id: String,
        method: String,
        uri: String,
        body: serde_json::Value,
    },
    ResponseChunk {
        timestamp: i64,
        id: String,
        chunk: String,
    },
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let (tx, _) = broadcast::channel(config.analysis.broadcast_channel_size);
        
        Self {
            config: Arc::new(config),
            client: Client::new(),
            injections: Arc::new(DashMap::new()),
            analysis_tx: tx,
        }
    }
}
