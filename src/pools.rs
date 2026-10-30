//! Pools: explicit virtual models that route to whichever member is available
//! (ADR-0026).
//!
//! A client asks for a pool by name (`model: "auto"`, `"auto/<pool>"` or the
//! pinned route `/auto/v1/...`); eavs picks the first available member in the
//! pool's order. `provider/*` members follow what a probed runtime serves right
//! now, so a loadout switch needs no configuration change. A member that fails
//! before the first byte is cooled down and the next member gets the request.
//! Concrete model names never fall back: pools are the only place eavs chooses
//! a model for the client.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::AppState;

/// Provider id reserved for pool requests.
pub const RESERVED: &str = "auto";
/// How long a member that failed a request is skipped.
const COOLDOWN: Duration = Duration::from_secs(30);
/// How long a conversation stays on its member without traffic.
const STICKY_TTL: Duration = Duration::from_secs(60 * 60);
/// Bound on remembered conversations.
const STICKY_MAX: usize = 10_000;
/// Members tried for one request before the last failure is returned.
const MAX_ATTEMPTS: usize = 4;
/// A probe older than this many intervals no longer vouches for a runtime.
const PROBE_FRESHNESS: u32 = 3;

/// One `[pools.<name>]` table.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Ordered `provider/model` routes; `provider/*` follows what the runtime
    /// serves right now.
    pub members: Vec<String>,
}

/// A concrete destination: provider and the model id it serves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Route {
    pub provider: String,
    pub model: String,
}

impl Route {
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

enum Member<'a> {
    Any(&'a str),
    Exact(&'a str, &'a str),
}

fn member(spec: &str) -> Option<Member<'_>> {
    let (provider, model) = spec.split_once('/')?;
    match (provider.is_empty(), model) {
        (true, _) | (_, "") => None,
        (false, "*") => Some(Member::Any(provider)),
        (false, model) => Some(Member::Exact(provider, model)),
    }
}

fn member_provider(spec: &str) -> Option<&str> {
    match member(spec)? {
        Member::Any(provider) | Member::Exact(provider, _) => Some(provider),
    }
}

/// The pool a request asks for, if any. `pinned` is the provider from the
/// path (`/{provider}/v1/...`) or the `X-Provider` header.
pub fn requested(pinned: Option<&str>, model: &str) -> Option<String> {
    if let Some(provider) = pinned {
        if !provider.eq_ignore_ascii_case(RESERVED) {
            return None;
        }
        return Some(match model {
            "" | "unknown" | RESERVED => RESERVED.to_owned(),
            model => model.strip_prefix("auto/").unwrap_or(model).to_owned(),
        });
    }
    if model == RESERVED {
        return Some(RESERVED.to_owned());
    }
    model
        .strip_prefix("auto/")
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// What eavs knows about one member provider right now.
#[derive(Debug, Clone, Default)]
pub struct ProviderView {
    /// Configured provider name (the route's provider).
    pub name: String,
    /// Configured model shortlist.
    pub configured: Vec<String>,
    /// For probed providers: what the runtime serves (empty when the last
    /// probe failed or is stale). `None` for providers that are not probed.
    pub served: Option<Vec<String>>,
    /// Models deactivated by an operator, maintenance key or probe.
    pub unavailable: HashSet<String>,
}

/// Members usable now, in pool order. `usable` adds the caller's key scope,
/// cooldowns and per-request exclusions.
pub fn candidates(
    pool: &PoolConfig,
    providers: &HashMap<String, ProviderView>,
    usable: impl Fn(&Route) -> bool,
) -> Vec<Route> {
    let mut out: Vec<Route> = Vec::new();
    for spec in &pool.members {
        let Some(member) = member(spec) else { continue };
        let (view, models) = match member {
            Member::Any(provider) => {
                let Some(view) = providers.get(provider) else {
                    continue;
                };
                let models = view
                    .served
                    .clone()
                    .unwrap_or_else(|| view.configured.clone());
                (view, models)
            }
            Member::Exact(provider, model) => {
                let Some(view) = providers.get(provider) else {
                    continue;
                };
                if view
                    .served
                    .as_ref()
                    .is_some_and(|served| !served.iter().any(|m| m == model))
                {
                    continue;
                }
                (view, vec![model.to_owned()])
            }
        };
        for model in models {
            if view.unavailable.contains(&model) {
                continue;
            }
            let route = Route {
                provider: view.name.clone(),
                model,
            };
            if !out.contains(&route) && usable(&route) {
                out.push(route);
            }
        }
    }
    out
}

/// The member a request goes to: the conversation's member while it is still
/// a candidate, otherwise the first candidate.
pub fn pick<'a>(candidates: &'a [Route], sticky: Option<&Route>) -> Option<&'a Route> {
    sticky
        .and_then(|route| candidates.iter().find(|c| *c == route))
        .or_else(|| candidates.first())
}

/// Request outcomes that move a pool request to the next member.
const fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 404 | 408 | 429 | 500 | 502 | 503 | 504)
}

/// Process-local pool state: cooldowns and conversation stickiness.
#[derive(Debug, Default)]
pub struct PoolRuntime {
    cooldown: Mutex<HashMap<Route, Instant>>,
    sticky: Mutex<HashMap<String, (Route, Instant)>>,
}

impl PoolRuntime {
    fn cooling(&self, route: &Route) -> bool {
        let Ok(mut cooldown) = self.cooldown.lock() else {
            return false;
        };
        let now = Instant::now();
        cooldown.retain(|_, until| *until > now);
        cooldown.contains_key(route)
    }

    fn cool(&self, route: &Route) {
        if let Ok(mut cooldown) = self.cooldown.lock() {
            cooldown.insert(route.clone(), Instant::now() + COOLDOWN);
        }
    }

    fn sticky(&self, key: &str) -> Option<Route> {
        let sticky = self.sticky.lock().ok()?;
        sticky
            .get(key)
            .filter(|(_, at)| at.elapsed() < STICKY_TTL)
            .map(|(route, _)| route.clone())
    }

    fn remember(&self, key: String, route: Route) {
        let Ok(mut sticky) = self.sticky.lock() else {
            return;
        };
        if sticky.len() >= STICKY_MAX {
            sticky.retain(|_, (_, at)| at.elapsed() < STICKY_TTL);
            if sticky.len() >= STICKY_MAX {
                sticky.clear();
            }
        }
        sticky.insert(key, (route, Instant::now()));
    }
}

/// Current knowledge about every provider a pool names.
async fn view(state: &AppState, pool: &PoolConfig) -> HashMap<String, ProviderView> {
    let probes = state.availability.probe_snapshot().await;
    let mut views: HashMap<String, ProviderView> = HashMap::new();
    for spec in &pool.members {
        let Some(name) = member_provider(spec) else {
            continue;
        };
        if views.contains_key(name) || name.eq_ignore_ascii_case(RESERVED) {
            continue;
        }
        // Only the provider of exactly this name; never a fallback.
        let Some(provider) = state
            .resolve_provider(name)
            .filter(|p| p.resolved_name.eq_ignore_ascii_case(name))
        else {
            continue;
        };
        let configured: Vec<String> = provider
            .config
            .models
            .iter()
            .map(|m| m.id.clone())
            .collect();
        let served = provider
            .config
            .probe
            .as_ref()
            .filter(|probe| probe.enabled)
            .map(|probe| {
                probes
                    .get(&provider.resolved_name)
                    .filter(|status| {
                        let age = chrono::Utc::now() - status.at;
                        status.ok
                            && age
                                .to_std()
                                .is_ok_and(|age| age <= probe.resolved_interval() * PROBE_FRESHNESS)
                    })
                    .map(|status| status.served.clone())
                    .unwrap_or_default()
            });
        let mut models: Vec<String> = configured.clone();
        models.extend(served.iter().flatten().cloned());
        for spec in &pool.members {
            if let Some(Member::Exact(p, m)) = member(spec) {
                if p == name {
                    models.push(m.to_owned());
                }
            }
        }
        let mut unavailable = HashSet::new();
        for model in models {
            if state
                .availability
                .unavailable(&provider.resolved_name, &model)
                .await
                .is_some()
            {
                unavailable.insert(model);
            }
        }
        views.insert(
            name.to_owned(),
            ProviderView {
                name: provider.resolved_name.clone(),
                configured,
                served,
                unavailable,
            },
        );
    }
    views
}

/// The caller's model/provider scope, without touching rate limits.
async fn key_scope(state: &AppState, headers: &HeaderMap) -> Option<crate::keys::KeyPermissions> {
    let token = bearer(headers)?;
    if !crate::keys::is_virtual_key(token) {
        return None;
    }
    let store = state.get_key_store()?;
    let key = store.lookup_for_auth(token).await.ok()??;
    Some(key.permissions)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn permits(scope: Option<&crate::keys::KeyPermissions>, route: &Route) -> bool {
    scope.is_none_or(|scope| {
        scope.is_provider_allowed(&route.provider) && scope.is_model_allowed(&route.model)
    })
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({"error": {"message": message, "type": code, "code": code}})),
    )
        .into_response()
}

fn annotate(response: &mut Response, pool: &str, route: &Route) {
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(pool) {
        headers.insert("x-eavs-pool", value);
    }
    if let Ok(value) = HeaderValue::from_str(&route.label()) {
        headers.insert("x-eavs-route", value);
    }
}

/// Whether this request could be a pool request without reading its body.
pub fn may_apply(state: &AppState, pinned: Option<&str>) -> bool {
    pinned.map_or_else(
        || !state.config.pools.is_empty(),
        |provider| provider.eq_ignore_ascii_case(RESERVED),
    )
}

/// Everything decided about a pool request before the first member is tried.
struct Plan {
    name: String,
    pool: PoolConfig,
    scope: Option<crate::keys::KeyPermissions>,
    views: HashMap<String, ProviderView>,
    sticky_key: Option<String>,
    sticky: Option<Route>,
}

/// Serve a request that may name a pool. Requests that do not are passed to
/// the normal handler unchanged.
pub async fn serve(
    state: AppState,
    req: Request<Body>,
    path_provider: Option<String>,
) -> Result<Response, Response> {
    let header_provider = req
        .headers()
        .get("x-provider")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let pinned = path_provider.clone().or(header_provider);
    let encoded = req
        .headers()
        .contains_key(axum::http::header::CONTENT_ENCODING);
    if encoded && path_provider.is_none() {
        // Compressed bodies are decoded by the main handler; only an explicit
        // `/auto/...` route marks them as pool requests.
        return crate::proxy::proxy_handler_pinned(state, req, path_provider).await;
    }
    let (parts, bytes) = read_body(&state, req).await?;
    let json_body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let model = json_body.get("model").and_then(Value::as_str).unwrap_or("");
    let Some(name) = requested(pinned.as_deref(), model) else {
        let req = Request::from_parts(parts, Body::from(bytes));
        return crate::proxy::proxy_handler_pinned(state, req, path_provider).await;
    };
    let Some(pool) = state.config.pools.get(&name).cloned() else {
        return Err(unknown_pool(&state, &name));
    };
    if json_body.is_null() || encoded {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Pool requests need an uncompressed JSON body with a model field",
        ));
    }
    let sticky_key = crate::client_identity::client_session(&parts.headers).map(|session| {
        use sha2::{Digest, Sha256};
        let caller = bearer(&parts.headers).unwrap_or("");
        hex::encode(Sha256::digest(
            format!("{caller}\0{name}\0{session}").as_bytes(),
        ))
    });
    let plan = Plan {
        scope: key_scope(&state, &parts.headers).await,
        views: view(&state, &pool).await,
        sticky: sticky_key
            .as_deref()
            .and_then(|key| state.pools.sticky(key)),
        sticky_key,
        name,
        pool,
    };
    dispatch(&state, &parts, json_body, plan).await
}

async fn read_body(
    state: &AppState,
    req: Request<Body>,
) -> Result<(axum::http::request::Parts, Bytes), Response> {
    let limit = match state.config.server.max_body_size {
        0 => usize::MAX,
        limit => limit,
    };
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, limit).await.map_err(|_| {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            &format!("Request body too large. Maximum size: {limit} bytes"),
        )
    })?;
    Ok((parts, bytes))
}

fn unknown_pool(state: &AppState, name: &str) -> Response {
    let mut known: Vec<&str> = state.config.pools.keys().map(String::as_str).collect();
    known.sort_unstable();
    let known = if known.is_empty() {
        "none".to_owned()
    } else {
        known.join(", ")
    };
    error(
        StatusCode::NOT_FOUND,
        "unknown_pool",
        &format!("Unknown pool '{name}'. Configured pools: {known}. Add [pools.{name}] to the configuration."),
    )
}

/// Try members in order until one answers without a retryable failure.
async fn dispatch(
    state: &AppState,
    parts: &axum::http::request::Parts,
    mut json_body: Value,
    plan: Plan,
) -> Result<Response, Response> {
    let mut excluded: HashSet<Route> = HashSet::new();
    let mut last: Option<(Result<Response, Response>, Route)> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let candidates = candidates(&plan.pool, &plan.views, |route| {
            permits(plan.scope.as_ref(), route)
                && !state.pools.cooling(route)
                && !excluded.contains(route)
        });
        let Some(route) = pick(&candidates, plan.sticky.as_ref()).cloned() else {
            break;
        };
        json_body["model"] = Value::String(route.model.clone());
        let Some(request) = member_request(parts, &route, &json_body) else {
            break;
        };
        let mut outcome = crate::proxy::proxy_handler_pinned(
            state.clone(),
            request,
            Some(route.provider.clone()),
        )
        .await;
        let status = match &outcome {
            Ok(response) | Err(response) => response.status(),
        };
        if retryable(status) && attempt + 1 < MAX_ATTEMPTS {
            tracing::info!(pool = %plan.name, route = %route.label(), %status, "Pool member failed; trying the next one");
            state.pools.cool(&route);
            excluded.insert(route.clone());
            last = Some((outcome, route));
            continue;
        }
        match &mut outcome {
            Ok(response) | Err(response) => annotate(response, &plan.name, &route),
        }
        if status.is_success() {
            if let Some(key) = plan.sticky_key {
                state.pools.remember(key, route);
            }
        } else if retryable(status) {
            state.pools.cool(&route);
        }
        return outcome;
    }
    if let Some((mut outcome, route)) = last {
        match &mut outcome {
            Ok(response) | Err(response) => annotate(response, &plan.name, &route),
        }
        return outcome;
    }
    Err(error(
        StatusCode::SERVICE_UNAVAILABLE,
        "no_available_model",
        &format!(
            "No member of pool '{}' is available right now (members: {}). Check `GET /v1/eavs/pools`.",
            plan.name,
            plan.pool.members.join(", ")
        ),
    ))
}

/// The request for one member: same method, headers and extensions, the
/// member's model in the body, and `/auto/...` rewritten to the provider.
fn member_request(
    parts: &axum::http::request::Parts,
    route: &Route,
    json_body: &Value,
) -> Option<Request<Body>> {
    let body = Bytes::from(serde_json::to_vec(json_body).ok()?);
    let mut builder = Request::builder()
        .method(parts.method.clone())
        .uri(member_uri(&parts.uri, &route.provider))
        .version(parts.version);
    if let Some(headers) = builder.headers_mut() {
        headers.clone_from(&parts.headers);
        headers.remove("x-provider");
        headers.remove(axum::http::header::CONTENT_LENGTH);
    }
    if let Some(extensions) = builder.extensions_mut() {
        extensions.clone_from(&parts.extensions);
    }
    builder.body(Body::from(body)).ok()
}

/// The member's request path: `/auto/...` becomes `/{provider}/...`; other
/// paths are kept.
fn member_uri(uri: &axum::http::Uri, provider: &str) -> axum::http::Uri {
    let path_and_query = uri.path_and_query().map_or("/", |p| p.as_str());
    let rewritten = path_and_query.strip_prefix("/auto/").map_or_else(
        || path_and_query.to_owned(),
        |rest| format!("/{provider}/{rest}"),
    );
    rewritten.parse().unwrap_or_else(|_| uri.clone())
}

#[derive(Serialize)]
struct PoolStatus {
    name: String,
    members: Vec<String>,
    /// Members usable by this key right now, in selection order.
    candidates: Vec<String>,
    /// Where a new conversation would go.
    pick: Option<String>,
}

/// `GET /v1/eavs/pools`: each pool with its members and current candidates,
/// filtered by the caller's key scope.
pub async fn status_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "missing_api_key",
            "A virtual key is required",
        );
    };
    let key = match state.get_key_store() {
        Some(store) if crate::keys::is_virtual_key(token) => {
            store.lookup_for_auth(token).await.ok().flatten()
        }
        _ => None,
    };
    let Some(key) = key.filter(|key| key.is_valid() && !key.permissions.maintenance) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid_api_key",
            "Invalid or inactive virtual key",
        );
    };
    let pools: BTreeMap<String, PoolConfig> = state.config.pools.clone().into_iter().collect();
    let mut out = Vec::new();
    for (name, pool) in pools {
        let views = view(&state, &pool).await;
        let candidates = candidates(&pool, &views, |route| {
            permits(Some(&key.permissions), route) && !state.pools.cooling(route)
        });
        out.push(PoolStatus {
            name,
            members: pool.members.clone(),
            pick: candidates.first().map(Route::label),
            candidates: candidates.iter().map(Route::label).collect(),
        });
    }
    let mut response = Json(json!({ "schema_version": 1, "pools": out })).into_response();
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("private, no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(members: &[&str]) -> PoolConfig {
        PoolConfig {
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    fn views(entries: Vec<ProviderView>) -> HashMap<String, ProviderView> {
        entries.into_iter().map(|v| (v.name.clone(), v)).collect()
    }

    fn probed(name: &str, served: &[&str]) -> ProviderView {
        ProviderView {
            name: name.to_owned(),
            served: Some(served.iter().map(|m| (*m).to_owned()).collect()),
            ..ProviderView::default()
        }
    }

    fn labels(routes: &[Route]) -> Vec<String> {
        routes.iter().map(Route::label).collect()
    }

    #[test]
    fn requested_pools_are_explicit() {
        assert_eq!(requested(None, "auto").as_deref(), Some("auto"));
        assert_eq!(requested(None, "auto/vision").as_deref(), Some("vision"));
        assert_eq!(
            requested(None, "gpt-5.4"),
            None,
            "concrete names never become pools"
        );
        assert_eq!(requested(None, "auto/"), None);
        assert_eq!(requested(Some("auto"), "coding").as_deref(), Some("coding"));
        assert_eq!(requested(Some("AUTO"), "").as_deref(), Some("auto"));
        assert_eq!(
            requested(Some("rtx6000"), "auto"),
            None,
            "a pinned provider wins"
        );
    }

    #[test]
    fn wildcard_follows_the_runtime_and_order_is_kept() {
        let pool = pool(&["rtx6000/*", "qwen/*", "remote/deepseek"]);
        let mut remote = ProviderView {
            name: "remote".to_owned(),
            configured: vec!["deepseek".to_owned()],
            ..ProviderView::default()
        };
        let all = views(vec![
            probed("rtx6000", &["glm"]),
            probed("qwen", &[]),
            remote.clone(),
        ]);
        assert_eq!(
            labels(&candidates(&pool, &all, |_| true)),
            ["rtx6000/glm", "remote/deepseek"]
        );

        // The loadout switched: rtx6000 now serves something else.
        let switched = views(vec![
            probed("rtx6000", &["kimi"]),
            probed("qwen", &["qwen3.8"]),
            remote.clone(),
        ]);
        assert_eq!(
            labels(&candidates(&pool, &switched, |_| true)),
            ["rtx6000/kimi", "qwen/qwen3.8", "remote/deepseek"]
        );

        // A deactivated model is skipped; so are routes the caller may not use.
        remote.unavailable.insert("deepseek".to_owned());
        let limited = views(vec![
            probed("rtx6000", &["kimi"]),
            probed("qwen", &["qwen3.8"]),
            remote,
        ]);
        let only_qwen = candidates(&pool, &limited, |r| r.provider == "qwen");
        assert_eq!(labels(&only_qwen), ["qwen/qwen3.8"]);
    }

    #[test]
    fn exact_members_of_probed_providers_must_be_served() {
        let pool = pool(&["rtx6000/glm", "rtx6000/kimi"]);
        let all = views(vec![probed("rtx6000", &["kimi"])]);
        assert_eq!(labels(&candidates(&pool, &all, |_| true)), ["rtx6000/kimi"]);
        assert!(
            candidates(&pool, &HashMap::new(), |_| true).is_empty(),
            "unknown providers are skipped"
        );
    }

    #[test]
    fn conversations_stay_on_their_member_while_it_is_available() {
        let a = Route {
            provider: "a".into(),
            model: "m".into(),
        };
        let b = Route {
            provider: "b".into(),
            model: "m".into(),
        };
        let both = vec![a.clone(), b.clone()];
        assert_eq!(pick(&both, Some(&b)), Some(&b));
        assert_eq!(pick(&both, None), Some(&a));
        assert_eq!(
            pick(std::slice::from_ref(&a), Some(&b)),
            Some(&a),
            "a vanished member is replaced"
        );
        assert_eq!(pick(&[], Some(&b)), None);
    }

    #[test]
    fn member_uri_rewrites_only_the_auto_prefix() {
        let uri: axum::http::Uri = "/auto/v1/chat/completions?x=1".parse().unwrap_or_default();
        assert_eq!(
            member_uri(&uri, "rtx6000").to_string(),
            "/rtx6000/v1/chat/completions?x=1"
        );
        let plain: axum::http::Uri = "/v1/chat/completions".parse().unwrap_or_default();
        assert_eq!(
            member_uri(&plain, "rtx6000").to_string(),
            "/v1/chat/completions"
        );
    }

    #[test]
    fn cooldowns_and_stickiness_expire() {
        let runtime = PoolRuntime::default();
        let route = Route {
            provider: "a".into(),
            model: "m".into(),
        };
        assert!(!runtime.cooling(&route));
        runtime.cool(&route);
        assert!(runtime.cooling(&route));
        runtime.remember("conv".into(), route.clone());
        assert_eq!(runtime.sticky("conv"), Some(route));
        assert_eq!(runtime.sticky("other"), None);
    }
}
