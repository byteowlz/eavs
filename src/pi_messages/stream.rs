//! Owned, pull-driven native stream conversion. Dropping the body drops upstream.
use super::{
    framing::{event, Framer},
    native,
    wire::{Counts, Wire},
};
use crate::{
    keys::{pricing::ModelPricing, ValidatedKey},
    provider::ProviderType,
    state::AppState,
};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue},
    response::Response,
};
use bytes::Bytes;
use futures::{stream::BoxStream, StreamExt};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub struct Admission {
    pub state: AppState,
    pub key: Option<ValidatedKey>,
    pub provider: String,
    pub model: String,
    pub kind: ProviderType,
    pub pricing: ModelPricing,
    pub oauth_tools: bool,
}
struct Ledger {
    state: AppState,
    key: Option<ValidatedKey>,
    provider: String,
    model: String,
    observed: Arc<Mutex<Option<(Counts, f64)>>>,
}
impl Drop for Ledger {
    fn drop(&mut self) {
        let Some(key) = self.key.clone() else { return };
        let observed = self.observed.lock().ok().and_then(|v| *v);
        let state = self.state.clone();
        let model = self.model.clone();
        let provider = self.provider.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(validator) = state.get_key_validator() {
                    if let Some((counts, cost)) = observed {
                        validator
                            .record_usage(
                                &key.key_hash,
                                counts.input,
                                counts.output,
                                counts.read,
                                counts.write,
                                cost,
                                &model,
                                &provider,
                            )
                            .await;
                    } else {
                        validator
                            .record_request(&key.key_hash, &model, &provider)
                            .await;
                    }
                }
            });
        }
    }
}
struct Machine {
    upstream: BoxStream<'static, Result<Bytes, std::io::Error>>,
    framer: Framer,
    wire: Wire,
    pricing: ModelPricing,
    queue: VecDeque<Value>,
    done: bool,
    sse: bool,
    json: Vec<u8>,
    ledger: Ledger,
}
impl Machine {
    fn observe(&self) {
        if let (Some(mut counts), Ok(usage)) = (self.wire.counts, self.wire.usage(&self.pricing)) {
            let Some(input) = usage["input"].as_u64().and_then(|v| u32::try_from(v).ok()) else {
                return;
            };
            counts.input = input;
            if let Some(cost) = usage["cost"]["total"].as_f64() {
                if let Ok(mut value) = self.ledger.observed.lock() {
                    *value = Some((counts, cost));
                }
            }
        }
    }
    fn fail(&mut self) {
        self.observe();
        self.queue.push_back(self.wire.failure(&self.pricing));
        self.done = true;
    }
    fn finish(&mut self) {
        let mut output = Vec::new();
        if self.wire.finish(&self.pricing, &mut output).is_err() {
            self.fail();
        } else {
            self.observe();
            self.queue.extend(output);
            self.done = true;
        }
    }
    fn chunk(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if !self.sse {
            if self.json.len() + bytes.len() > 16 * 1024 * 1024 {
                return Err(());
            }
            self.json.extend_from_slice(bytes);
            return Ok(());
        }
        if bytes.len() > 1024 * 1024 {
            return Err(());
        }
        for frame in self.framer.push(bytes)? {
            if let Some((kind, data)) = event(&frame) {
                let mut output = Vec::new();
                native::parse(&mut self.wire, &kind, &data, &mut output)?;
                self.queue.extend(output);
                self.observe();
                if self.wire.terminal && self.wire.counts.is_some() {
                    self.finish();
                    break;
                }
            }
        }
        Ok(())
    }
    fn eof(&mut self) {
        if self.sse {
            if self.framer.finish().is_err() {
                self.fail();
            } else {
                self.finish();
            }
        } else {
            let mut output = Vec::new();
            let parsed: Result<Value, _> = serde_json::from_slice(&self.json);
            if parsed
                .map_err(|_| ())
                .and_then(|v| native::complete(&mut self.wire, &v, &mut output))
                .is_err()
            {
                self.fail();
            } else {
                self.queue.extend(output);
                self.finish();
            }
        }
    }
}

pub fn response(
    admission: Admission,
    upstream: BoxStream<'static, Result<Bytes, std::io::Error>>,
    headers: &HeaderMap,
) -> Response {
    let sse = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let mut wire = Wire::new(
        admission.provider.clone(),
        admission.model.clone(),
        super::canonical::api(admission.kind),
    );
    wire.oauth_tools = admission.oauth_tools;
    let ledger = Ledger {
        state: admission.state,
        key: admission.key,
        provider: admission.provider,
        model: admission.model,
        observed: Arc::new(Mutex::new(None)),
    };
    let machine = Machine {
        upstream,
        framer: Framer::default(),
        wire,
        pricing: admission.pricing,
        queue: VecDeque::new(),
        done: false,
        sse,
        json: Vec::new(),
        ledger,
    };
    let stream = futures::stream::unfold(machine, |mut machine| async move {
        loop {
            if let Some(value) = machine.queue.pop_front() {
                return Some((
                    Ok::<_, std::io::Error>(Bytes::from(format!("data: {value}\n\n"))),
                    machine,
                ));
            }
            if machine.done {
                return None;
            }
            match machine.upstream.next().await {
                Some(Ok(bytes)) => {
                    if machine.chunk(&bytes).is_err() {
                        machine.fail();
                    }
                }
                Some(Err(_)) => machine.fail(),
                None => machine.eof(),
            }
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        "cache-control",
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert(
        "x-eavs-pi-delivery",
        HeaderValue::from_static(if sse {
            "streaming"
        } else {
            "buffered-upstream"
        }),
    );
    headers.insert("x-eavs-pi-cost", HeaderValue::from_static("estimated"));
    response
}
