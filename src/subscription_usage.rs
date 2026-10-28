//! Subscription and credit monitoring for upstream accounts.
//!
//! The credential owner (ADR-0002, ADR-0022) periodically reads each account's
//! own usage endpoint — the same plain reads the vendors' CLIs make for their
//! usage displays — and keeps the last good result. Nothing here generates
//! tokens or spends quota. Clients (web console, GPUI app, CLI) only read the
//! cached model.
//!
//! Rules:
//! - Unknown is unknown: a window the vendor did not report is absent, never 0.
//! - A real measured zero (e.g. an exhausted credit balance) is reported as 0.
//! - On 429 the last good meters are kept and marked stale; the vendor's
//!   `retry-after` is honoured before the account is asked again.
//! - Borrowed credentials are read but never refreshed; an expired token is
//!   reported as such.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use futures::StreamExt;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::upstream::{Upstream, UpstreamRequest};

/// How often the background loop revisits every account.
const POLL_INTERVAL: Duration = Duration::from_mins(5);
/// Minimum spacing between two reads of one account (manual refreshes too).
const MIN_REFRESH: Duration = Duration::from_mins(1);
/// Default back-off after a 429 without `retry-after`.
const RATE_LIMIT_BACKOFF: Duration = Duration::from_mins(15);
/// Bound on a usage response body.
const MAX_BODY: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const ANTHROPIC_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const OPENCODE_GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const OPENROUTER_API: &str = "https://openrouter.ai/api/v1";
const ZAI_QUOTA_PATH: &str = "/api/monitor/usage/quota/limit";
const ZAI_SUBSCRIPTION_PATH: &str = "/api/biz/subscription/list";

const SESSION_SECS: u64 = 5 * 3600;
const WEEK_SECS: u64 = 7 * 86_400;
const MONTH_SECS: u64 = 30 * 86_400;

/// What a meter measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterUnit {
    /// `used` is a percentage of the window (0-100), `limit` is 100.
    Percent,
    /// US dollars.
    Usd,
    /// Vendor credits.
    Credits,
    /// A plain count.
    Count,
}

/// One usage meter of an account.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Meter {
    /// Stable id, e.g. `session`, `weekly`, `extra_usage`, `balance`.
    pub id: String,
    pub label: String,
    pub unit: MeterUnit,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Remaining amount, when the vendor reports a balance rather than usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
    /// Window length, for pace displays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_secs: Option<u64>,
}

impl Meter {
    fn percent(
        id: &str,
        label: &str,
        used: f64,
        resets_at: Option<DateTime<Utc>>,
        period: u64,
    ) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            unit: MeterUnit::Percent,
            used: Some(used.clamp(0.0, 1000.0)),
            limit: Some(100.0),
            remaining: None,
            resets_at,
            period_secs: Some(period),
        }
    }
}

/// Freshness of an account's usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    /// Read successfully on the last attempt.
    Ok,
    /// Showing the last good read; the latest attempt failed or was rate limited.
    Stale,
    /// No usable read yet.
    Error,
    /// The credential was rejected or has expired.
    Unauthenticated,
    /// The account has no plan to meter (e.g. no subscription on this login).
    NoPlan,
}

/// Usage of one upstream account.
#[derive(Debug, Clone, Serialize)]
pub struct AccountUsage {
    /// Stable id, e.g. `oauth/anthropic/default/default` or `provider/opencode-go`.
    pub id: String,
    /// Upstream vendor: `anthropic`, `openai-codex`, `opencode-go`, `openrouter`, `zai`.
    pub vendor: String,
    /// The eavs provider (or OAuth login) this account backs.
    pub provider: String,
    /// Account label (OAuth) or `default`.
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub status: UsageStatus,
    /// Human-readable reason for a non-ok status; never contains credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When the meters were last read successfully.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<DateTime<Utc>>,
    pub meters: Vec<Meter>,
}

/// A parsed vendor response: plan name plus meters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub plan: Option<String>,
    pub meters: Vec<Meter>,
}

// ---------------------------------------------------------------------------
// Pure parsers (one per vendor). Each takes the vendor's JSON and returns only
// what the vendor actually reported.
// ---------------------------------------------------------------------------

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// RFC3339 text or epoch seconds/milliseconds.
fn instant(value: Option<&Value>) -> Option<DateTime<Utc>> {
    if let Some(Value::String(text)) = value {
        if let Ok(parsed) = DateTime::parse_from_rfc3339(text.trim()) {
            return Some(parsed.with_timezone(&Utc));
        }
    }
    let raw = number(value)?;
    let millis = if raw.abs() < 1e10 { raw * 1000.0 } else { raw };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "epoch milliseconds fit in i64"
    )]
    Utc.timestamp_millis_opt(millis as i64).single()
}

fn plan_label(raw: Option<&Value>) -> Option<String> {
    let text = raw?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.strip_prefix("claude_").unwrap_or(text);
    let mut chars = text.chars();
    let first = chars.next()?;
    Some(
        first
            .to_uppercase()
            .chain(chars)
            .collect::<String>()
            .replace('_', " "),
    )
}

/// Claude subscription: `GET /api/oauth/usage`.
pub fn parse_anthropic(body: &Value) -> Parsed {
    let mut meters = Vec::new();
    for (key, id, label, period) in [
        ("five_hour", "session", "Session", SESSION_SECS),
        ("seven_day", "weekly", "Weekly", WEEK_SECS),
        (
            "seven_day_sonnet",
            "weekly_sonnet",
            "Sonnet weekly",
            WEEK_SECS,
        ),
        ("seven_day_opus", "weekly_opus", "Opus weekly", WEEK_SECS),
    ] {
        let Some(window) = body.get(key).filter(|w| w.is_object()) else {
            continue;
        };
        if let Some(used) = number(window.get("utilization")) {
            meters.push(Meter::percent(
                id,
                label,
                used,
                instant(window.get("resets_at")),
                period,
            ));
        }
    }
    // Model-scoped weekly limits moved into a `limits` array.
    if let Some(limits) = body.get("limits").and_then(Value::as_array) {
        for limit in limits {
            if limit.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
                continue;
            }
            let Some(model) = limit
                .pointer("/scope/model/display_name")
                .and_then(Value::as_str)
            else {
                continue;
            };
            if let Some(used) = number(limit.get("percent")) {
                let id = format!("weekly_{}", model.to_ascii_lowercase().replace(' ', "_"));
                if meters.iter().any(|m| m.id == id) {
                    continue;
                }
                meters.push(Meter::percent(
                    &id,
                    &format!("{model} weekly"),
                    used,
                    instant(limit.get("resets_at")),
                    WEEK_SECS,
                ));
            }
        }
    }
    // Extra usage: credits in cents against an optional monthly cap.
    if let Some(extra) = body.get("extra_usage") {
        if extra.get("is_enabled").and_then(Value::as_bool) == Some(true) {
            if let Some(used_cents) = number(extra.get("used_credits")) {
                let limit = number(extra.get("monthly_limit"))
                    .filter(|cents| *cents > 0.0)
                    .map(|cents| cents / 100.0);
                meters.push(Meter {
                    id: "extra_usage".to_owned(),
                    label: "Extra usage".to_owned(),
                    unit: MeterUnit::Usd,
                    used: Some(used_cents / 100.0),
                    limit,
                    remaining: limit.map(|cap| (cap - used_cents / 100.0).max(0.0)),
                    resets_at: None,
                    period_secs: Some(MONTH_SECS),
                });
            }
        }
    }
    Parsed { plan: None, meters }
}

/// Codex (`ChatGPT` plan): `GET /backend-api/wham/usage`. Windows are
/// classified by their duration, not their primary/secondary slot.
pub fn parse_codex(body: &Value, headers: &HeaderMap) -> Parsed {
    let mut meters = Vec::new();
    let header_percent = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    let rate_limit = body.get("rate_limit");
    for (slot, header, fallback) in [
        ("primary_window", "x-codex-primary-used-percent", "session"),
        (
            "secondary_window",
            "x-codex-secondary-used-percent",
            "weekly",
        ),
    ] {
        let window = rate_limit
            .and_then(|r| r.get(slot))
            .filter(|w| w.is_object());
        let used = window
            .and_then(|w| number(w.get("used_percent")))
            .or_else(|| header_percent(header));
        let Some(used) = used else { continue };
        let seconds = window.and_then(|w| number(w.get("limit_window_seconds")));
        let kind = match seconds {
            Some(s) if s >= 86_400.0 => "weekly",
            Some(_) => "session",
            None => fallback,
        };
        if meters.iter().any(|m: &Meter| m.id == kind) {
            continue;
        }
        let resets_at = window.and_then(|w| instant(w.get("reset_at"))).or_else(|| {
            let after = window.and_then(|w| number(w.get("reset_after_seconds")))?;
            #[expect(clippy::cast_possible_truncation, reason = "seconds until reset")]
            Some(Utc::now() + chrono::Duration::seconds(after as i64))
        });
        let (label, period) = if kind == "weekly" {
            ("Weekly", WEEK_SECS)
        } else {
            ("Session", SESSION_SECS)
        };
        meters.push(Meter::percent(kind, label, used, resets_at, period));
    }
    let credits = body
        .get("credits")
        .and_then(|c| {
            number(c.get("balance")).or_else(|| {
                (c.get("has_credits").and_then(Value::as_bool) == Some(false)).then_some(0.0)
            })
        })
        .or_else(|| header_percent("x-codex-credits-balance"));
    if let Some(balance) = credits {
        meters.push(Meter {
            id: "credits".to_owned(),
            label: "Credits".to_owned(),
            unit: MeterUnit::Credits,
            used: None,
            limit: None,
            remaining: Some(balance.max(0.0).floor()),
            resets_at: None,
            period_secs: None,
        });
    }
    Parsed {
        plan: plan_label(body.get("plan_type")),
        meters,
    }
}

/// `OpenCode` Go: `GET /zen/go/v1/usage` — rolling, weekly and monthly percents.
pub fn parse_opencode_go(body: &Value) -> Parsed {
    let mut meters = Vec::new();
    if let Some(usage) = body.get("usage") {
        for (key, id, label, period) in [
            ("rolling", "session", "Session", SESSION_SECS),
            ("weekly", "weekly", "Weekly", WEEK_SECS),
            ("monthly", "monthly", "Monthly", MONTH_SECS),
        ] {
            let Some(window) = usage.get(key) else {
                continue;
            };
            if let Some(used) = number(window.get("percent")) {
                meters.push(Meter::percent(
                    id,
                    label,
                    used,
                    instant(window.get("resetsAt")),
                    period,
                ));
            }
        }
    }
    Parsed {
        plan: Some("Go".to_owned()),
        meters,
    }
}

/// `OpenRouter`: `/credits` (account balance) and `/key` (per-key cap/spend).
pub fn parse_openrouter(credits: Option<&Value>, key: Option<&Value>) -> Parsed {
    let mut meters = Vec::new();
    if let Some(data) = credits.and_then(|c| c.get("data")) {
        if let Some(total_usage) = number(data.get("total_usage")) {
            let total_credits = number(data.get("total_credits")).unwrap_or(0.0).max(0.0);
            meters.push(Meter {
                id: "balance".to_owned(),
                label: "Credits".to_owned(),
                unit: MeterUnit::Usd,
                used: Some(total_usage),
                limit: Some(total_credits),
                remaining: Some((total_credits - total_usage).max(0.0)),
                resets_at: None,
                period_secs: None,
            });
        }
    }
    let mut plan = None;
    if let Some(data) = key.and_then(|k| k.get("data")) {
        if let Some(limit) = number(data.get("limit")).filter(|l| *l > 0.0) {
            let remaining = number(data.get("limit_remaining"));
            meters.push(Meter {
                id: "key_limit".to_owned(),
                label: "Key limit".to_owned(),
                unit: MeterUnit::Usd,
                used: remaining.map(|r| (limit - r).max(0.0)),
                limit: Some(limit),
                remaining,
                resets_at: None,
                period_secs: None,
            });
        }
        if data.get("is_free_tier").and_then(Value::as_bool) == Some(true) {
            plan = Some("Free tier".to_owned());
        }
    }
    Parsed { plan, meters }
}

/// Z.ai GLM Coding Plan: `/api/monitor/usage/quota/limit` (+ plan name).
pub fn parse_zai(quota: &Value, subscription: Option<&Value>) -> Parsed {
    let mut meters = Vec::new();
    let container = quota.get("data").unwrap_or(quota);
    if let Some(limits) = container.get("limits").and_then(Value::as_array) {
        for entry in limits {
            let kind = entry
                .get("type")
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "CREDIT_LIMIT" | "TOKENS_LIMIT" => {
                    let unit = number(entry.get("unit"));
                    let count = number(entry.get("number")).unwrap_or(1.0);
                    let period = match unit {
                        Some(u) if (u - 3.0).abs() < f64::EPSILON => 3600.0 * count,
                        Some(u) if (u - 4.0).abs() < f64::EPSILON => 86_400.0 * count,
                        Some(u) if (u - 6.0).abs() < f64::EPSILON => 604_800.0 * count,
                        Some(u) if (u - 5.0).abs() < f64::EPSILON => 2_592_000.0 * count,
                        _ => continue,
                    };
                    let (id, label) = if period < 86_400.0 {
                        ("session", "Session")
                    } else {
                        ("weekly", "Weekly")
                    };
                    if let Some(used) = number(entry.get("percentage")) {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "window seconds"
                        )]
                        let period = period as u64;
                        meters.push(Meter::percent(
                            id,
                            label,
                            used,
                            instant(entry.get("nextResetTime")),
                            period,
                        ));
                    }
                }
                "TIME_LIMIT" => {
                    let used =
                        number(entry.get("currentValue")).or_else(|| number(entry.get("usage")));
                    let limit =
                        number(entry.get("usage")).filter(|_| entry.get("currentValue").is_some());
                    if used.is_some() {
                        meters.push(Meter {
                            id: "web_search".to_owned(),
                            label: "Web search".to_owned(),
                            unit: MeterUnit::Count,
                            used,
                            limit,
                            remaining: number(entry.get("remaining")),
                            resets_at: instant(entry.get("nextResetTime")),
                            period_secs: Some(MONTH_SECS),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    let plan = subscription
        .and_then(|s| s.get("data"))
        .and_then(Value::as_array)
        .and_then(|list| list.first())
        .and_then(|first| first.get("productName"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Parsed { plan, meters }
}

// ---------------------------------------------------------------------------
// Targets and fetching
// ---------------------------------------------------------------------------

/// Where an account's usage is read from.
#[derive(Debug, Clone)]
pub enum Target {
    /// A subscription login in the OAuth store.
    Anthropic {
        user: String,
        account: String,
    },
    Codex {
        user: String,
        account: String,
    },
    /// A configured provider using an API key.
    OpencodeGo {
        provider: String,
    },
    OpenRouter {
        provider: String,
    },
    Zai {
        provider: String,
        base: String,
    },
}

impl Target {
    fn id(&self) -> String {
        match self {
            Self::Anthropic { user, account } => format!("oauth/anthropic/{user}/{account}"),
            Self::Codex { user, account } => format!("oauth/openai-codex/{user}/{account}"),
            Self::OpencodeGo { provider }
            | Self::OpenRouter { provider }
            | Self::Zai { provider, .. } => {
                format!("provider/{provider}")
            }
        }
    }

    const fn vendor(&self) -> &'static str {
        match self {
            Self::Anthropic { .. } => "anthropic",
            Self::Codex { .. } => "openai-codex",
            Self::OpencodeGo { .. } => "opencode-go",
            Self::OpenRouter { .. } => "openrouter",
            Self::Zai { .. } => "zai",
        }
    }

    fn provider_and_account(&self) -> (String, String) {
        match self {
            Self::Anthropic { user, account } => (format!("anthropic ({user})"), account.clone()),
            Self::Codex { user, account } => (format!("openai-codex ({user})"), account.clone()),
            Self::OpencodeGo { provider }
            | Self::OpenRouter { provider }
            | Self::Zai { provider, .. } => (provider.clone(), "default".to_owned()),
        }
    }
}

/// Outcome of one read.
#[derive(Debug)]
pub enum Fetch {
    Ok(Parsed),
    RateLimited(Option<Duration>),
    Unauthenticated(String),
    NoPlan(String),
    Failed(String),
}

struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

async fn get(
    upstream: &dyn Upstream,
    url: &str,
    headers: &[(&'static str, String)],
) -> Result<Response, String> {
    let mut map = HeaderMap::new();
    map.insert(
        http::header::ACCEPT,
        HeaderValue::from_static("application/json"),
    );
    for (name, value) in headers {
        let value = HeaderValue::from_str(value).map_err(|_| "invalid header value".to_owned())?;
        map.insert(*name, value);
    }
    let request = UpstreamRequest {
        method: Method::GET,
        url: url.to_owned(),
        headers: map,
        body: Bytes::new(),
    };
    let response = tokio::time::timeout(REQUEST_TIMEOUT, upstream.send(request))
        .await
        .map_err(|_| "timed out".to_owned())?
        .map_err(|error| format!("request failed: {}", error.kind()))?;
    let mut body = Vec::new();
    let mut stream = response.body;
    while let Some(chunk) = tokio::time::timeout(REQUEST_TIMEOUT, stream.next())
        .await
        .map_err(|_| "timed out reading the response".to_owned())?
    {
        let chunk = chunk.map_err(|error| format!("read failed: {}", error.kind()))?;
        if body.len() + chunk.len() > MAX_BODY {
            return Err("response too large".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Response {
        status: response.status,
        headers: response.headers,
        body: Bytes::from(body),
    })
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Classify a non-success status; `None` means success.
fn classify(response: &Response) -> Option<Fetch> {
    match response.status {
        s if s.is_success() => None,
        StatusCode::TOO_MANY_REQUESTS => Some(Fetch::RateLimited(retry_after(&response.headers))),
        StatusCode::UNAUTHORIZED => Some(Fetch::Unauthenticated(
            "credential rejected; sign in again".to_owned(),
        )),
        StatusCode::FORBIDDEN => Some(Fetch::NoPlan(
            "this login has no plan or lacks the scope to read usage".to_owned(),
        )),
        s => Some(Fetch::Failed(format!(
            "usage endpoint answered {}",
            s.as_u16()
        ))),
    }
}

fn json(response: &Response) -> Result<Value, Fetch> {
    serde_json::from_slice(&response.body)
        .map_err(|_| Fetch::Failed("unrecognized usage response".to_owned()))
}

async fn fetch_anthropic(upstream: &dyn Upstream, token: &str) -> Fetch {
    let response = match get(
        upstream,
        ANTHROPIC_USAGE_URL,
        &[
            ("authorization", format!("Bearer {}", token.trim())),
            ("anthropic-beta", "oauth-2025-04-20".to_owned()),
        ],
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return Fetch::Failed(error),
    };
    if let Some(failure) = classify(&response) {
        return failure;
    }
    match json(&response) {
        Ok(body) => Fetch::Ok(parse_anthropic(&body)),
        Err(failure) => failure,
    }
}

async fn fetch_codex(upstream: &dyn Upstream, token: &str, account_id: Option<&str>) -> Fetch {
    let mut headers = vec![("authorization", format!("Bearer {}", token.trim()))];
    if let Some(account_id) = account_id.filter(|id| !id.is_empty()) {
        headers.push(("chatgpt-account-id", account_id.to_owned()));
    }
    let response = match get(upstream, CODEX_USAGE_URL, &headers).await {
        Ok(response) => response,
        Err(error) => return Fetch::Failed(error),
    };
    if let Some(failure) = classify(&response) {
        return failure;
    }
    match json(&response) {
        Ok(body) => Fetch::Ok(parse_codex(&body, &response.headers)),
        Err(failure) => failure,
    }
}

async fn fetch_bearer_json(upstream: &dyn Upstream, url: &str, key: &str) -> Result<Value, Fetch> {
    let response = get(
        upstream,
        url,
        &[("authorization", format!("Bearer {}", key.trim()))],
    )
    .await
    .map_err(Fetch::Failed)?;
    if let Some(failure) = classify(&response) {
        return Err(failure);
    }
    json(&response)
}

// ---------------------------------------------------------------------------
// Store and loop
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Entry {
    usage: AccountUsage,
    next_allowed: Instant,
}

/// Cached usage per account.
#[derive(Default)]
pub struct SubscriptionUsage {
    entries: RwLock<BTreeMap<String, Entry>>,
}

impl SubscriptionUsage {
    /// Current snapshot, sorted by account id.
    pub async fn snapshot(&self) -> Vec<AccountUsage> {
        self.entries
            .read()
            .await
            .values()
            .map(|entry| entry.usage.clone())
            .collect()
    }

    /// Fold one read into the cache. Failures keep the last good meters.
    pub async fn record(&self, target: &Target, fetch: Fetch, now: Instant) {
        let id = target.id();
        let (provider, account) = target.provider_and_account();
        let mut entries = self.entries.write().await;
        let previous = entries.remove(&id);
        let had_meters = previous
            .as_ref()
            .is_some_and(|entry| !entry.usage.meters.is_empty());
        let mut usage = previous.map_or_else(
            || AccountUsage {
                id: id.clone(),
                vendor: target.vendor().to_owned(),
                provider: provider.clone(),
                account: account.clone(),
                plan: None,
                status: UsageStatus::Error,
                message: None,
                observed_at: None,
                meters: Vec::new(),
            },
            |entry| entry.usage,
        );
        let mut next_allowed = now + MIN_REFRESH;
        match fetch {
            Fetch::Ok(parsed) => {
                usage.status = UsageStatus::Ok;
                usage.message = parsed
                    .meters
                    .is_empty()
                    .then(|| "the vendor reported no usage windows for this account".to_owned());
                if parsed.plan.is_some() {
                    usage.plan = parsed.plan;
                }
                usage.meters = parsed.meters;
                usage.observed_at = Some(Utc::now());
            }
            Fetch::RateLimited(wait) => {
                let wait = wait.unwrap_or(RATE_LIMIT_BACKOFF);
                next_allowed = now + wait.max(MIN_REFRESH);
                usage.status = if had_meters {
                    UsageStatus::Stale
                } else {
                    UsageStatus::Error
                };
                usage.message = Some(format!(
                    "rate limited by the vendor; retrying in about {} min",
                    wait.as_secs().div_ceil(60)
                ));
            }
            Fetch::Unauthenticated(message) => {
                usage.status = UsageStatus::Unauthenticated;
                usage.message = Some(message);
            }
            Fetch::NoPlan(message) => {
                usage.status = UsageStatus::NoPlan;
                usage.message = Some(message);
                usage.meters.clear();
            }
            Fetch::Failed(message) => {
                usage.status = if had_meters {
                    UsageStatus::Stale
                } else {
                    UsageStatus::Error
                };
                usage.message = Some(message);
            }
        }
        entries.insert(
            id,
            Entry {
                usage,
                next_allowed,
            },
        );
    }

    async fn due(&self, target: &Target, now: Instant) -> bool {
        self.entries
            .read()
            .await
            .get(&target.id())
            .is_none_or(|entry| entry.next_allowed <= now)
    }

    /// Drop accounts that are no longer configured.
    async fn retain(&self, targets: &[Target]) {
        let ids: std::collections::HashSet<String> = targets.iter().map(Target::id).collect();
        self.entries.write().await.retain(|id, _| ids.contains(id));
    }
}

/// Every account whose usage can be read: OAuth logins of the default user
/// and configured API-key providers of supported vendors.
pub async fn targets(state: &crate::state::AppState) -> Vec<(Target, Option<String>)> {
    let mut targets = Vec::new();
    if let Some(store) = state.get_oauth_store() {
        let user = state.oauth_default_user().to_owned();
        for (provider, kind) in [
            (crate::oauth::OAuthProvider::Anthropic, 0),
            (crate::oauth::OAuthProvider::OpenAICodex, 1),
        ] {
            if let Ok(Some(credentials)) = store.get_credentials(&user, provider).await {
                let account = credentials.account_label.clone();
                let target = if kind == 0 {
                    Target::Anthropic {
                        user: user.clone(),
                        account,
                    }
                } else {
                    Target::Codex {
                        user: user.clone(),
                        account,
                    }
                };
                targets.push((target, None));
            }
        }
    }
    for runtime in state.runtime_providers().await {
        let config = &runtime.config;
        let base = config.resolved_base_url();
        let host = url::Url::parse(&base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        let target = match crate::provider::ProviderType::try_from_str(&config.type_) {
            Some(crate::provider::ProviderType::OpenCodeGo) => Target::OpencodeGo {
                provider: runtime.resolved_name.clone(),
            },
            Some(crate::provider::ProviderType::OpenRouter) => Target::OpenRouter {
                provider: runtime.resolved_name.clone(),
            },
            _ if host == "api.z.ai" || host == "open.bigmodel.cn" => Target::Zai {
                provider: runtime.resolved_name.clone(),
                base: format!("https://{host}"),
            },
            _ => continue,
        };
        let key = config.resolved_api_key();
        if key.trim().is_empty() || key == "none" {
            continue;
        }
        targets.push((target, Some(key)));
    }
    targets
}

async fn read(state: &crate::state::AppState, target: &Target, key: Option<&str>) -> Fetch {
    match target {
        Target::Anthropic { .. } | Target::Codex { .. } => read_oauth(state, target).await,
        _ => match key {
            Some(key) => read_api_key(state.upstream.as_ref(), target, key).await,
            None => Fetch::Unauthenticated("no API key".to_owned()),
        },
    }
}

/// A subscription login: read with its current access token, never refresh.
async fn read_oauth(state: &crate::state::AppState, target: &Target) -> Fetch {
    let (Target::Anthropic { user, account } | Target::Codex { user, account }) = target else {
        return Fetch::Failed("not an OAuth account".to_owned());
    };
    let Some(store) = state.get_oauth_store() else {
        return Fetch::Failed("OAuth store unavailable".to_owned());
    };
    let anthropic = matches!(target, Target::Anthropic { .. });
    let provider = if anthropic {
        crate::oauth::OAuthProvider::Anthropic
    } else {
        crate::oauth::OAuthProvider::OpenAICodex
    };
    let credentials = match store
        .get_credentials_for_account(user, provider, account)
        .await
    {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return Fetch::Unauthenticated("not signed in".to_owned()),
        Err(_) => return Fetch::Failed("OAuth store read failed".to_owned()),
    };
    if credentials.is_expired(0) {
        return Fetch::Unauthenticated(
            "access token expired; it refreshes on the next request through eavs".to_owned(),
        );
    }
    let upstream = state.upstream.as_ref();
    if anthropic {
        fetch_anthropic(upstream, &credentials.access_token).await
    } else {
        // The proxy derives the ChatGPT account id from the access token the
        // same way when it calls the Codex backend.
        let account_id = crate::proxy::extract_openai_account_id(&credentials.access_token);
        fetch_codex(upstream, &credentials.access_token, account_id.as_deref()).await
    }
}

/// A configured provider using an API key.
async fn read_api_key(upstream: &dyn Upstream, target: &Target, key: &str) -> Fetch {
    match target {
        Target::OpencodeGo { .. } => {
            match fetch_bearer_json(upstream, OPENCODE_GO_USAGE_URL, key).await {
                Ok(body) => Fetch::Ok(parse_opencode_go(&body)),
                Err(failure) => failure,
            }
        }
        Target::OpenRouter { .. } => {
            let credits =
                fetch_bearer_json(upstream, &format!("{OPENROUTER_API}/credits"), key).await;
            let key_info = fetch_bearer_json(upstream, &format!("{OPENROUTER_API}/key"), key).await;
            match (credits, key_info) {
                (Err(failure), Err(_)) => failure,
                (credits, key_info) => Fetch::Ok(parse_openrouter(
                    credits.as_ref().ok(),
                    key_info.as_ref().ok(),
                )),
            }
        }
        Target::Zai { base, .. } => {
            match fetch_bearer_json(upstream, &format!("{base}{ZAI_QUOTA_PATH}"), key).await {
                Ok(quota) => {
                    let plan =
                        fetch_bearer_json(upstream, &format!("{base}{ZAI_SUBSCRIPTION_PATH}"), key)
                            .await
                            .ok();
                    Fetch::Ok(parse_zai(&quota, plan.as_ref()))
                }
                Err(failure) => failure,
            }
        }
        Target::Anthropic { .. } | Target::Codex { .. } => {
            Fetch::Failed("not an API-key account".to_owned())
        }
    }
}

/// Read every due account once. `force` ignores the regular poll schedule but
/// never the per-account minimum spacing or a vendor's `retry-after`.
pub async fn refresh(state: &crate::state::AppState) {
    let targets = targets(state).await;
    let tracker = state.subscription_usage.clone();
    tracker
        .retain(&targets.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>())
        .await;
    for (target, key) in targets {
        let now = Instant::now();
        if !tracker.due(&target, now).await {
            continue;
        }
        let fetch = read(state, &target, key.as_deref()).await;
        debug!(account = %target.id(), outcome = ?std::mem::discriminant(&fetch), "Subscription usage read");
        tracker.record(&target, fetch, now).await;
    }
}

/// Background loop: read all accounts every `POLL_INTERVAL`.
pub fn spawn(state: Arc<crate::state::AppState>) {
    tokio::spawn(async move {
        info!("Subscription usage monitor started");
        loop {
            refresh(&state).await;
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
}

#[cfg(test)]
#[expect(clippy::unwrap_used, reason = "fixture assertions")]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_windows_scoped_limits_and_extra_usage() {
        let body = json!({
            "five_hour": {"utilization": 23.0, "resets_at": "2026-10-27T18:00:00Z"},
            "seven_day": {"utilization": 69, "resets_at": "2026-10-30T10:00:00+00:00"},
            "seven_day_sonnet": null,
            "limits": [{"kind": "weekly_scoped", "percent": 12,
                        "scope": {"model": {"display_name": "Fable"}},
                        "resets_at": "2026-10-30T10:00:00Z"}],
            "extra_usage": {"is_enabled": true, "used_credits": 1234, "monthly_limit": 5000}
        });
        let parsed = parse_anthropic(&body);
        let ids: Vec<&str> = parsed.meters.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "weekly_fable", "extra_usage"]);
        assert_eq!(parsed.meters[0].used, Some(23.0));
        assert!(parsed.meters[0].resets_at.is_some());
        let extra = &parsed.meters[3];
        assert_eq!(extra.unit, MeterUnit::Usd);
        assert_eq!(extra.used, Some(12.34));
        assert_eq!(extra.limit, Some(50.0));
        assert_eq!(extra.remaining, Some(37.66));
    }

    #[test]
    fn unreported_windows_are_absent_not_zero() {
        let parsed =
            parse_anthropic(&json!({"five_hour": null, "extra_usage": {"is_enabled": false}}));
        assert!(parsed.meters.is_empty());
    }

    #[test]
    fn codex_classifies_by_duration_and_reads_credits() {
        let body = json!({
            "plan_type": "pro",
            "rate_limit": {
                // A sole weekly limit moved into the primary slot.
                "primary_window": {"used_percent": 41, "limit_window_seconds": 604_800, "reset_after_seconds": 3600},
                "secondary_window": null
            },
            "credits": {"balance": "12.9", "has_credits": true}
        });
        let parsed = parse_codex(&body, &HeaderMap::new());
        assert_eq!(parsed.plan.as_deref(), Some("Pro"));
        assert_eq!(parsed.meters[0].id, "weekly");
        assert_eq!(parsed.meters[0].used, Some(41.0));
        assert_eq!(parsed.meters[1].remaining, Some(12.0));
    }

    #[test]
    fn codex_exhausted_credits_are_a_real_zero() {
        let parsed = parse_codex(
            &json!({"credits": {"has_credits": false}}),
            &HeaderMap::new(),
        );
        assert_eq!(parsed.meters[0].remaining, Some(0.0));
    }

    #[test]
    fn opencode_go_windows() {
        let parsed = parse_opencode_go(&json!({"usage": {
            "rolling": {"percent": 3, "resetsAt": "2026-10-27T18:00:00Z"},
            "weekly": {"percent": 40, "resetsAt": "2026-11-02T00:00:00Z"},
            "monthly": {"percent": 10, "resetsAt": "2026-11-15T00:00:00Z"}}}));
        let ids: Vec<&str> = parsed.meters.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "monthly"]);
    }

    #[test]
    fn openrouter_balance_and_key_cap() {
        let parsed = parse_openrouter(
            Some(&json!({"data": {"total_credits": 50, "total_usage": 12.5}})),
            Some(&json!({"data": {"limit": 20, "limit_remaining": 15, "is_free_tier": false}})),
        );
        assert_eq!(parsed.meters[0].remaining, Some(37.5));
        assert_eq!(parsed.meters[1].used, Some(5.0));
    }

    #[test]
    fn zai_quota_windows_and_plan() {
        let parsed = parse_zai(
            &json!({"data": {"limits": [
                {"type": "CREDIT_LIMIT", "unit": 3, "number": 5, "percentage": 17, "nextResetTime": 1_793_000_000_000_i64},
                {"type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 55},
                {"type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 100, "currentValue": 7, "remaining": 93}
            ]}}),
            Some(&json!({"data": [{"productName": "GLM Coding Max"}]})),
        );
        let ids: Vec<&str> = parsed.meters.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["session", "weekly", "web_search"]);
        assert_eq!(parsed.plan.as_deref(), Some("GLM Coding Max"));
        assert_eq!(parsed.meters[2].used, Some(7.0));
        assert_eq!(parsed.meters[2].limit, Some(100.0));
    }

    #[tokio::test]
    async fn rate_limit_keeps_last_good_meters_and_backs_off() {
        let tracker = SubscriptionUsage::default();
        let target = Target::OpencodeGo {
            provider: "opencode-go".to_owned(),
        };
        let now = Instant::now();
        tracker
            .record(
                &target,
                Fetch::Ok(parse_opencode_go(
                    &json!({"usage": {"weekly": {"percent": 40}}}),
                )),
                now,
            )
            .await;
        let later = now + MIN_REFRESH;
        tracker
            .record(
                &target,
                Fetch::RateLimited(Some(Duration::from_mins(20))),
                later,
            )
            .await;
        let snapshot = tracker.snapshot().await;
        assert_eq!(snapshot[0].status, UsageStatus::Stale);
        assert_eq!(snapshot[0].meters[0].used, Some(40.0));
        assert!(!tracker.due(&target, later + Duration::from_mins(10)).await);
        assert!(
            tracker
                .due(&target, later + Duration::from_secs(1201))
                .await
        );
    }

    #[tokio::test]
    async fn first_failure_is_an_error_not_zero_usage() {
        let tracker = SubscriptionUsage::default();
        let target = Target::OpenRouter {
            provider: "openrouter".to_owned(),
        };
        tracker
            .record(
                &target,
                Fetch::Failed("timed out".to_owned()),
                Instant::now(),
            )
            .await;
        let snapshot = tracker.snapshot().await;
        assert_eq!(snapshot[0].status, UsageStatus::Error);
        assert!(snapshot[0].meters.is_empty());
        assert!(snapshot[0].observed_at.is_none());
    }

    /// Records requests; answers every GET with the same JSON body.
    struct Recording {
        body: Value,
        seen: std::sync::Mutex<Vec<(String, Option<String>)>>,
    }

    impl Upstream for Recording {
        fn send(
            &self,
            request: UpstreamRequest,
        ) -> futures::future::BoxFuture<'_, Result<crate::upstream::UpstreamResponse, std::io::Error>>
        {
            let auth = request
                .headers
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((request.url, auth));
            }
            let body = Bytes::from(serde_json::to_vec(&self.body).unwrap_or_default());
            Box::pin(async move {
                Ok(crate::upstream::UpstreamResponse {
                    status: StatusCode::OK,
                    headers: HeaderMap::new(),
                    body: futures::stream::once(async move { Ok(body) }).boxed(),
                })
            })
        }
    }

    #[tokio::test]
    async fn refresh_reads_configured_api_key_providers_only() {
        let upstream = Arc::new(Recording {
            body: json!({"usage": {"weekly": {"percent": 40, "resetsAt": "2026-11-02T00:00:00Z"}}}),
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let mut config = crate::config::AppConfig::with_defaults();
        config.providers.clear();
        config.providers.insert(
            "ocgo".to_owned(),
            crate::config::ProviderConfig {
                type_: "opencode-go".to_owned(),
                base_url: "https://opencode.ai/zen/go/v1".to_owned(),
                api_key: "sk-go-synthetic".to_owned(),
                ..Default::default()
            },
        );
        config.providers.insert(
            "local".to_owned(),
            crate::config::ProviderConfig {
                type_: "openai-compatible".to_owned(),
                base_url: "http://127.0.0.1:8080/v1".to_owned(),
                api_key: "none".to_owned(),
                ..Default::default()
            },
        );
        let state = crate::state::AppState::new_with_upstream(config, upstream.clone());
        refresh(&state).await;
        let seen = upstream
            .seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default();
        assert_eq!(
            seen,
            vec![(
                OPENCODE_GO_USAGE_URL.to_owned(),
                Some("Bearer sk-go-synthetic".to_owned())
            )]
        );
        let snapshot = state.subscription_usage.snapshot().await;
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, "provider/ocgo");
        assert_eq!(snapshot[0].status, UsageStatus::Ok);
        assert_eq!(snapshot[0].meters[0].used, Some(40.0));
        // The credential never appears in the read model.
        assert!(!serde_json::to_string(&snapshot)
            .unwrap()
            .contains("sk-go-synthetic"));
        // A second refresh inside the minimum spacing does not ask again.
        refresh(&state).await;
        assert_eq!(
            upstream
                .seen
                .lock()
                .map(|seen| seen.len())
                .unwrap_or_default(),
            1
        );
    }
}
