use crate::state::{AppState, InjectionPayload};
use axum::{
    extract::{Path, State},
    response::sse::{Event, Sse},
    Json, http::StatusCode,
};
use futures::stream::Stream;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

pub async fn inject_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Json(payload): Json<InjectionPayload>,
) -> StatusCode {
    let mut entry = state.injections.entry(conversation_id).or_default();
    entry.extend(payload.messages);
    StatusCode::OK
}

pub async fn clear_handler(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
) -> StatusCode {
    state.injections.remove(&conversation_id);
    StatusCode::OK
}

pub async fn logs_stream_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    let rx = state.analysis_tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|res| {
        match res {
            Ok(event) => {
                let data = serde_json::to_string(&event).unwrap_or_default();
                Ok(Event::default().data(data))
            },
            Err(_) => Ok(Event::default().event("error").data("Lagged")),
        }
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, LoggingConfig, AnalysisConfig};
    use crate::state::Injection;
    use std::collections::HashMap;

    fn mock_state() -> AppState {
        let config = AppConfig {
            upstream: HashMap::new(),
            logging: LoggingConfig { sink: "stdout".to_string() },
            analysis: AnalysisConfig { enabled: true, broadcast_channel_size: 10 },
        };
        AppState::new(config)
    }

    #[tokio::test]
    async fn test_inject_and_clear() {
        let state = mock_state();
        let conversation_id = "test-conv".to_string();
        
        // Test Inject logic (simulating handler)
        let payload = InjectionPayload {
            messages: vec![Injection { role: "user".to_string(), content: "hi".to_string() }]
        };
        
        {
            let mut entry = state.injections.entry(conversation_id.clone()).or_default();
            entry.extend(payload.messages);
        }

        assert!(state.injections.contains_key(&conversation_id));
        assert_eq!(state.injections.get(&conversation_id).unwrap().len(), 1);

        // Test Clear logic
        state.injections.remove(&conversation_id);
        assert!(!state.injections.contains_key(&conversation_id));
    }
}
