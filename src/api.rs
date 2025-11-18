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
