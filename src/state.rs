use crate::config::{AppConfig, StateConfig};
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub client: Client,
    /// Conversation state store with TTL support
    pub conversations: Arc<ConversationStore>,
    /// Legacy injections map (for backward compatibility)
    pub injections: Arc<DashMap<String, Vec<Injection>>>,
    /// Broadcast channel for analysis logs
    pub analysis_tx: broadcast::Sender<AnalysisEvent>,
}

/// A conversation entry with metadata for TTL tracking.
#[derive(Debug, Clone)]
pub struct ConversationEntry {
    pub injections: Vec<Injection>,
    pub metadata: ConversationMetadata,
    #[allow(dead_code)]
    pub created_at: Instant,
    pub last_accessed: Instant,
}

/// Metadata about a conversation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConversationMetadata {
    /// Custom tags for the conversation
    #[serde(default)]
    pub tags: Vec<String>,
    /// Provider to use for this conversation
    pub provider: Option<String>,
    /// Model override for this conversation
    pub model: Option<String>,
    /// Request count for this conversation
    #[serde(default)]
    pub request_count: u64,
}

/// Thread-safe conversation store with TTL and cleanup.
pub struct ConversationStore {
    entries: DashMap<String, ConversationEntry>,
    config: StateConfig,
    stats: ConversationStats,
}

/// Statistics for the conversation store.
#[derive(Default)]
pub struct ConversationStats {
    pub total_created: AtomicU64,
    pub total_expired: AtomicU64,
    pub total_evicted: AtomicU64,
}

impl ConversationStore {
    pub fn new(config: StateConfig) -> Self {
        Self {
            entries: DashMap::new(),
            config,
            stats: ConversationStats::default(),
        }
    }

    /// Get or create a conversation entry.
    pub fn get_or_create(&self, conversation_id: &str) -> ConversationEntry {
        let now = Instant::now();

        self.entries
            .entry(conversation_id.to_string())
            .and_modify(|entry| {
                entry.last_accessed = now;
            })
            .or_insert_with(|| {
                self.stats.total_created.fetch_add(1, Ordering::Relaxed);
                ConversationEntry {
                    injections: Vec::new(),
                    metadata: ConversationMetadata::default(),
                    created_at: now,
                    last_accessed: now,
                }
            })
            .clone()
    }

    /// Get a conversation entry if it exists.
    pub fn get(&self, conversation_id: &str) -> Option<ConversationEntry> {
        let now = Instant::now();
        self.entries.get_mut(conversation_id).map(|mut entry| {
            entry.last_accessed = now;
            entry.clone()
        })
    }

    /// Add injections to a conversation.
    pub fn add_injections(&self, conversation_id: &str, injections: Vec<Injection>) {
        let now = Instant::now();

        self.entries
            .entry(conversation_id.to_string())
            .and_modify(|entry| {
                entry.injections.extend(injections.clone());
                entry.last_accessed = now;
            })
            .or_insert_with(|| {
                self.stats.total_created.fetch_add(1, Ordering::Relaxed);
                ConversationEntry {
                    injections,
                    metadata: ConversationMetadata::default(),
                    created_at: now,
                    last_accessed: now,
                }
            });

        // Check if we need to evict old entries
        self.maybe_evict();
    }

    /// Take and remove injections for a conversation.
    pub fn take_injections(&self, conversation_id: &str) -> Vec<Injection> {
        if let Some(mut entry) = self.entries.get_mut(conversation_id) {
            entry.last_accessed = Instant::now();
            entry.metadata.request_count += 1;
            std::mem::take(&mut entry.injections)
        } else {
            Vec::new()
        }
    }

    /// Update conversation metadata.
    pub fn update_metadata<F>(&self, conversation_id: &str, f: F)
    where
        F: FnOnce(&mut ConversationMetadata),
    {
        if let Some(mut entry) = self.entries.get_mut(conversation_id) {
            f(&mut entry.metadata);
            entry.last_accessed = Instant::now();
        }
    }

    /// Clear a conversation.
    pub fn clear(&self, conversation_id: &str) {
        self.entries.remove(conversation_id);
    }

    /// Run cleanup to remove expired entries.
    pub fn cleanup(&self) {
        if self.config.ttl_secs == 0 {
            return; // No TTL configured
        }

        let ttl = Duration::from_secs(self.config.ttl_secs);
        let now = Instant::now();
        let mut expired = 0u64;

        self.entries.retain(|_, entry| {
            let age = now.duration_since(entry.last_accessed);
            if age > ttl {
                expired += 1;
                false
            } else {
                true
            }
        });

        if expired > 0 {
            self.stats.total_expired.fetch_add(expired, Ordering::Relaxed);
            tracing::debug!("Cleaned up {} expired conversations", expired);
        }
    }

    /// Evict oldest entries if over capacity.
    fn maybe_evict(&self) {
        if self.config.max_conversations == 0 {
            return; // No limit
        }

        let current_count = self.entries.len();
        if current_count <= self.config.max_conversations {
            return;
        }

        // Find and remove oldest entries
        let to_remove = current_count - self.config.max_conversations;
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), e.last_accessed))
            .collect();

        entries.sort_by_key(|(_, accessed)| *accessed);

        for (key, _) in entries.into_iter().take(to_remove) {
            self.entries.remove(&key);
            self.stats.total_evicted.fetch_add(1, Ordering::Relaxed);
        }

        tracing::debug!("Evicted {} conversations due to capacity limit", to_remove);
    }

    /// Get current statistics.
    pub fn stats(&self) -> (usize, u64, u64, u64) {
        (
            self.entries.len(),
            self.stats.total_created.load(Ordering::Relaxed),
            self.stats.total_expired.load(Ordering::Relaxed),
            self.stats.total_evicted.load(Ordering::Relaxed),
        )
    }

    /// List all conversation IDs.
    pub fn list_conversations(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.key().clone()).collect()
    }
}

/// Start the cleanup task for expired conversations.
pub fn start_cleanup_task(store: Arc<ConversationStore>, interval_secs: u64) -> mpsc::Sender<()> {
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    if interval_secs == 0 {
        return shutdown_tx; // Cleanup disabled
    }

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    store.cleanup();
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    });

    shutdown_tx
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
#[serde(tag = "type")]
pub enum AnalysisEvent {
    #[serde(rename = "request")]
    Request {
        timestamp: i64,
        id: String,
        method: String,
        uri: String,
        body: serde_json::Value,
    },
    #[serde(rename = "response_chunk")]
    ResponseChunk {
        timestamp: i64,
        id: String,
        chunk: String,
    },
    #[serde(rename = "response_complete")]
    #[allow(dead_code)]
    ResponseComplete {
        timestamp: i64,
        id: String,
        status: u16,
        duration_ms: i64,
    },
    #[serde(rename = "error")]
    #[allow(dead_code)]
    Error {
        timestamp: i64,
        id: String,
        error: String,
    },
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        let (tx, _) = broadcast::channel(config.analysis.broadcast_channel_size);
        let conversations = Arc::new(ConversationStore::new(config.state.clone()));

        Self {
            config: Arc::new(config),
            client: Client::new(),
            conversations,
            injections: Arc::new(DashMap::new()), // Legacy support
            analysis_tx: tx,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AnalysisConfig, LoggingConfig, ProviderConfig, ServerConfig};
    use std::collections::HashMap;
    use std::thread::sleep;
    use std::time::Duration;

    fn mock_config() -> AppConfig {
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                api_key: "env:OPENAI_API_KEY".to_string(),
                base_url: "https://api.openai.com/v1".to_string(),
                ..Default::default()
            },
        );

        AppConfig {
            server: ServerConfig::default(),
            providers,
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 16,
            },
            state: StateConfig::default(),
        }
    }

    fn mock_state_config() -> StateConfig {
        StateConfig {
            enabled: true,
            ttl_secs: 1, // 1 second for testing
            cleanup_interval_secs: 1,
            max_conversations: 3,
        }
    }

    #[test]
    fn test_app_state_new() {
        let config = mock_config();
        let state = AppState::new(config);

        assert!(state.injections.is_empty());
        assert!(state.config.analysis.enabled);
        assert_eq!(state.config.analysis.broadcast_channel_size, 16);
    }

    #[test]
    fn test_conversation_store_get_or_create() {
        let store = ConversationStore::new(mock_state_config());

        let entry1 = store.get_or_create("conv-1");
        assert!(entry1.injections.is_empty());

        let entry2 = store.get_or_create("conv-1");
        assert!(entry2.injections.is_empty());

        let (count, created, _, _) = store.stats();
        assert_eq!(count, 1);
        assert_eq!(created, 1); // Only created once
    }

    #[test]
    fn test_conversation_store_add_injections() {
        let store = ConversationStore::new(mock_state_config());

        store.add_injections(
            "conv-1",
            vec![Injection {
                role: "system".to_string(),
                content: "Be helpful".to_string(),
            }],
        );

        let entry = store.get("conv-1").unwrap();
        assert_eq!(entry.injections.len(), 1);
        assert_eq!(entry.injections[0].role, "system");
    }

    #[test]
    fn test_conversation_store_take_injections() {
        let store = ConversationStore::new(mock_state_config());

        store.add_injections(
            "conv-1",
            vec![Injection {
                role: "system".to_string(),
                content: "Be helpful".to_string(),
            }],
        );

        let injections = store.take_injections("conv-1");
        assert_eq!(injections.len(), 1);

        // Second take should be empty
        let injections2 = store.take_injections("conv-1");
        assert!(injections2.is_empty());

        // But conversation should still exist with incremented request count
        let entry = store.get("conv-1").unwrap();
        assert_eq!(entry.metadata.request_count, 2);
    }

    #[test]
    fn test_conversation_store_ttl_cleanup() {
        let store = ConversationStore::new(StateConfig {
            enabled: true,
            ttl_secs: 1,
            cleanup_interval_secs: 1,
            max_conversations: 100,
        });

        store.add_injections(
            "conv-1",
            vec![Injection {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
        );

        assert!(store.get("conv-1").is_some());

        // Wait for TTL to expire
        sleep(Duration::from_millis(1100));
        store.cleanup();

        assert!(store.get("conv-1").is_none());

        let (_, _, expired, _) = store.stats();
        assert_eq!(expired, 1);
    }

    #[test]
    fn test_conversation_store_eviction() {
        let store = ConversationStore::new(StateConfig {
            enabled: true,
            ttl_secs: 0, // No TTL
            cleanup_interval_secs: 0,
            max_conversations: 2,
        });

        store.add_injections("conv-1", vec![]);
        sleep(Duration::from_millis(10));
        store.add_injections("conv-2", vec![]);
        sleep(Duration::from_millis(10));
        store.add_injections("conv-3", vec![]); // Should trigger eviction

        let (count, _, _, evicted) = store.stats();
        assert_eq!(count, 2);
        assert_eq!(evicted, 1);

        // conv-1 should be evicted (oldest)
        assert!(store.get("conv-1").is_none());
        assert!(store.get("conv-2").is_some());
        assert!(store.get("conv-3").is_some());
    }

    #[test]
    fn test_conversation_store_update_metadata() {
        let store = ConversationStore::new(mock_state_config());

        store.add_injections("conv-1", vec![]);

        store.update_metadata("conv-1", |meta| {
            meta.provider = Some("anthropic".to_string());
            meta.tags.push("test".to_string());
        });

        let entry = store.get("conv-1").unwrap();
        assert_eq!(entry.metadata.provider, Some("anthropic".to_string()));
        assert!(entry.metadata.tags.contains(&"test".to_string()));
    }

    #[test]
    fn test_conversation_store_list() {
        let store = ConversationStore::new(mock_state_config());

        store.add_injections("conv-1", vec![]);
        store.add_injections("conv-2", vec![]);

        let conversations = store.list_conversations();
        assert_eq!(conversations.len(), 2);
        assert!(conversations.contains(&"conv-1".to_string()));
        assert!(conversations.contains(&"conv-2".to_string()));
    }

    #[test]
    fn test_legacy_injections_concurrent_access() {
        let config = mock_config();
        let state = AppState::new(config);

        // Legacy injections still work
        state.injections.insert(
            "conv-1".to_string(),
            vec![Injection {
                role: "system".to_string(),
                content: "Be helpful".to_string(),
            }],
        );
        state.injections.insert(
            "conv-2".to_string(),
            vec![Injection {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
        );

        assert_eq!(state.injections.len(), 2);
        assert!(state.injections.contains_key("conv-1"));
        assert!(state.injections.contains_key("conv-2"));

        let removed = state.injections.remove("conv-1");
        assert!(removed.is_some());
        assert_eq!(state.injections.len(), 1);
    }

    #[test]
    fn test_analysis_event_broadcast() {
        let config = mock_config();
        let state = AppState::new(config);

        let mut rx = state.analysis_tx.subscribe();

        let event = AnalysisEvent::Request {
            timestamp: 1234567890,
            id: "test-id".to_string(),
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            body: serde_json::json!({"model": "gpt-4"}),
        };

        state.analysis_tx.send(event.clone()).unwrap();

        let received = rx.try_recv().unwrap();
        match received {
            AnalysisEvent::Request {
                id, method, uri, ..
            } => {
                assert_eq!(id, "test-id");
                assert_eq!(method, "POST");
                assert_eq!(uri, "/v1/chat/completions");
            }
            _ => panic!("Expected Request event"),
        }
    }

    #[test]
    fn test_analysis_event_serialization() {
        let event = AnalysisEvent::Request {
            timestamp: 1234567890,
            id: "test-id".to_string(),
            method: "POST".to_string(),
            uri: "/v1/chat/completions".to_string(),
            body: serde_json::json!({"model": "gpt-4"}),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"request\""));
        assert!(json.contains("\"id\":\"test-id\""));
    }

    #[test]
    fn test_injection_serialization() {
        let injection = Injection {
            role: "system".to_string(),
            content: "You are a helpful assistant".to_string(),
        };

        let json = serde_json::to_string(&injection).unwrap();
        assert!(json.contains("system"));
        assert!(json.contains("You are a helpful assistant"));

        let deserialized: Injection = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, "system");
        assert_eq!(deserialized.content, "You are a helpful assistant");
    }

    #[test]
    fn test_injection_payload_serialization() {
        let payload = InjectionPayload {
            messages: vec![
                Injection {
                    role: "system".to_string(),
                    content: "System prompt".to_string(),
                },
                Injection {
                    role: "user".to_string(),
                    content: "User message".to_string(),
                },
            ],
        };

        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: InjectionPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.messages.len(), 2);
        assert_eq!(deserialized.messages[0].role, "system");
        assert_eq!(deserialized.messages[1].role, "user");
    }
}
