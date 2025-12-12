use crate::state::{AppState, ConversationMetadata, InjectionPayload};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, Sse},
    Json,
};
use futures::stream::Stream;
use serde::Serialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Inject messages into a conversation.
pub async fn inject_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<InjectionPayload>,
) -> StatusCode {
    state
        .conversations
        .add_injections(&conversation_id, payload.messages);
    StatusCode::OK
}

/// Clear a conversation's pending injections.
pub async fn clear_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> StatusCode {
    state.conversations.clear(&conversation_id);
    // Also clear legacy injections
    state.injections.remove(&conversation_id);
    StatusCode::OK
}

/// Stream analysis logs via SSE.
pub async fn logs_stream_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    let rx = state.analysis_tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|res| match res {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Ok(Event::default().data(data))
        }
        Err(_) => Ok(Event::default().event("error").data("Lagged")),
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Health check endpoint.
pub async fn health_handler() -> StatusCode {
    StatusCode::OK
}

/// List available providers.
pub async fn providers_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    let providers: Vec<String> = state.config.providers.keys().cloned().collect();
    Json(providers)
}

/// Response for conversation stats.
#[derive(Serialize)]
pub struct ConversationStatsResponse {
    pub active_conversations: usize,
    pub total_created: u64,
    pub total_expired: u64,
    pub total_evicted: u64,
}

/// Get conversation store statistics.
pub async fn stats_handler(State(state): State<AppState>) -> Json<ConversationStatsResponse> {
    let (active, created, expired, evicted) = state.conversations.stats();
    Json(ConversationStatsResponse {
        active_conversations: active,
        total_created: created,
        total_expired: expired,
        total_evicted: evicted,
    })
}

/// List all active conversations.
pub async fn conversations_handler(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.conversations.list_conversations())
}

/// Get conversation details.
#[derive(Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub metadata: ConversationMetadata,
    pub pending_injections: usize,
    pub request_count: u64,
}

pub async fn conversation_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> Result<Json<ConversationResponse>, StatusCode> {
    state
        .conversations
        .get(&conversation_id)
        .map(|entry| {
            Json(ConversationResponse {
                id: conversation_id,
                metadata: entry.metadata.clone(),
                pending_injections: entry.injections.len(),
                request_count: entry.metadata.request_count,
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

/// Update conversation metadata.
#[derive(serde::Deserialize)]
pub struct UpdateMetadataPayload {
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

pub async fn update_conversation_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<UpdateMetadataPayload>,
) -> StatusCode {
    // Ensure conversation exists
    state.conversations.get_or_create(&conversation_id);

    state
        .conversations
        .update_metadata(&conversation_id, |meta| {
            if let Some(provider) = payload.provider {
                meta.provider = Some(provider);
            }
            if let Some(model) = payload.model {
                meta.model = Some(model);
            }
            if let Some(tags) = payload.tags {
                meta.tags = tags;
            }
        });

    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AnalysisConfig, AppConfig, LoggingConfig, ProviderConfig, ServerConfig, StateConfig,
    };
    use crate::state::Injection;
    use std::collections::HashMap;

    fn mock_state() -> AppState {
        let config = AppConfig {
            server: ServerConfig::default(),
            providers: HashMap::new(),
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 10,
            },
            state: StateConfig::default(),
        };
        AppState::new(config)
    }

    fn mock_state_with_providers() -> AppState {
        let mut providers = HashMap::new();
        providers.insert(
            "default".to_string(),
            ProviderConfig {
                type_: "openai".to_string(),
                ..Default::default()
            },
        );
        providers.insert(
            "anthropic".to_string(),
            ProviderConfig {
                type_: "anthropic".to_string(),
                ..Default::default()
            },
        );

        let config = AppConfig {
            server: ServerConfig::default(),
            providers,
            upstream: HashMap::new(),
            logging: LoggingConfig::default(),
            analysis: AnalysisConfig {
                enabled: true,
                broadcast_channel_size: 10,
            },
            state: StateConfig::default(),
        };
        AppState::new(config)
    }

    #[tokio::test]
    async fn test_inject_and_clear() {
        let state = mock_state();
        let conversation_id = "test-conv".to_string();

        // Test injection via conversation store
        let payload = InjectionPayload {
            messages: vec![Injection {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
        };

        state
            .conversations
            .add_injections(&conversation_id, payload.messages);

        let entry = state.conversations.get(&conversation_id).unwrap();
        assert_eq!(entry.injections.len(), 1);

        // Test clear
        state.conversations.clear(&conversation_id);
        assert!(state.conversations.get(&conversation_id).is_none());
    }

    #[tokio::test]
    async fn test_providers_list() {
        let state = mock_state_with_providers();
        let mut providers: Vec<String> = state.config.providers.keys().cloned().collect();
        providers.sort();

        assert_eq!(providers.len(), 2);
        assert!(providers.contains(&"default".to_string()));
        assert!(providers.contains(&"anthropic".to_string()));
    }

    #[tokio::test]
    async fn test_conversation_stats() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.add_injections("conv-2", vec![]);

        let (active, created, _, _) = state.conversations.stats();
        assert_eq!(active, 2);
        assert_eq!(created, 2);
    }

    #[tokio::test]
    async fn test_list_conversations() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.add_injections("conv-2", vec![]);

        let conversations = state.conversations.list_conversations();
        assert_eq!(conversations.len(), 2);
    }

    #[tokio::test]
    async fn test_update_metadata() {
        let state = mock_state();

        state.conversations.add_injections("conv-1", vec![]);
        state.conversations.update_metadata("conv-1", |meta| {
            meta.provider = Some("anthropic".to_string());
            meta.tags.push("test".to_string());
        });

        let entry = state.conversations.get("conv-1").unwrap();
        assert_eq!(entry.metadata.provider, Some("anthropic".to_string()));
        assert!(entry.metadata.tags.contains(&"test".to_string()));
    }
}
