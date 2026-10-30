//! Explicit, file-provider-only routing publication. Never reconstruct `AppState`.
use crate::{
    config::{AppConfig, ProviderConfig},
    provider::ProviderType,
    state::AppState,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{watch, Mutex, OnceCell};

#[derive(Clone)]
pub struct Snapshot {
    pub generation: String,
    pub config: Arc<AppConfig>,
    /// Preserve the existing implicit catalog-default provider across additions.
    pub default_provider_hint: Option<String>,
}

type Loader = Arc<dyn Fn() -> Result<AppConfig, Failure> + Send + Sync>;
struct Staged {
    generation: String,
    base: String,
    config: Arc<AppConfig>,
    created: Instant,
}

pub struct RoutingState {
    head: watch::Sender<Snapshot>,
    source: OnceCell<Loader>,
    /// All HTTP provider ownership mutations use this coordinator too.
    pub changes: Mutex<()>,
    staged: Mutex<Option<Staged>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReloadRequest {
    #[serde(default = "preview_default")]
    pub dry_run: bool,
    pub expected_generation: Option<String>,
    pub expected_candidate_generation: Option<String>,
}
const fn preview_default() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    Unavailable,
    InvalidConfiguration,
    RestartRequired,
    InvalidProvider,
    InvalidDestination,
    PricingChange,
    NativeJobChange,
    SourceConflict,
    GenerationConflict,
    CandidateConflict,
    MissingExpectation,
}
impl Failure {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "reload_unavailable",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::RestartRequired => "restart_required",
            Self::InvalidProvider => "invalid_provider",
            Self::InvalidDestination => "invalid_destination",
            Self::PricingChange => "pricing_change_not_reloadable",
            Self::NativeJobChange => "native_job_route_not_reloadable",
            Self::SourceConflict => "provider_source_conflict",
            Self::GenerationConflict => "generation_conflict",
            Self::CandidateConflict => "candidate_changed_or_expired",
            Self::MissingExpectation => "preview_and_generations_required",
        }
    }
}

#[derive(Serialize)]
pub struct Change {
    pub provider: String,
    pub action: &'static str,
}
#[derive(Serialize)]
pub struct ReloadResult {
    pub scope: &'static str,
    pub status: &'static str,
    pub current_generation: String,
    pub candidate_generation: String,
    pub resulting_generation: String,
    pub changes: Vec<Change>,
}

impl RoutingState {
    pub fn new(config: Arc<AppConfig>) -> Self {
        let default_provider_hint = config
            .resolve_provider("default")
            .map(|p| p.resolved_name)
            .or_else(|| config.providers.keys().next().cloned());
        let (head, _) = watch::channel(Snapshot {
            generation: uuid::Uuid::new_v4().to_string(),
            config,
            default_provider_hint,
        });
        Self {
            head,
            source: OnceCell::new(),
            changes: Mutex::new(()),
            staged: Mutex::new(None),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.head.borrow().clone()
    }
    pub fn configure(
        &self,
        loader: impl Fn() -> Result<AppConfig, Failure> + Send + Sync + 'static,
    ) -> Result<(), Failure> {
        self.source
            .set(Arc::new(loader))
            .map_err(|_| Failure::Unavailable)
    }
    pub fn configured(&self) -> bool {
        self.source.get().is_some()
    }

    pub async fn reload(
        &self,
        state: &AppState,
        request: ReloadRequest,
    ) -> Result<ReloadResult, Failure> {
        let base = self.snapshot();
        if request
            .expected_generation
            .as_ref()
            .is_some_and(|id| id != &base.generation)
        {
            return Err(Failure::GenerationConflict);
        }
        if !request.dry_run
            && (request.expected_generation.is_none()
                || request.expected_candidate_generation.is_none())
        {
            return Err(Failure::MissingExpectation);
        }
        let loader = self.source.get().cloned().ok_or(Failure::Unavailable)?;
        // File parsing must neither block the reactor nor resolve credential references.
        let candidate = tokio::task::spawn_blocking(move || loader())
            .await
            .map_err(|_| Failure::Unavailable)??;
        let _guard = self.changes.lock().await;
        if self.snapshot().generation != base.generation {
            return Err(Failure::GenerationConflict);
        }
        validate(&base.config, &candidate)?;
        if let Some(store) = state.get_provider_store() {
            let entries = store
                .list_all_providers()
                .await
                .map_err(|_| Failure::Unavailable)?;
            for entry in entries {
                // Reject additions/removals which would switch the active declaration owner.
                let before = base.config.resolve_provider(&entry.name);
                let after = candidate.resolve_provider(&entry.name);
                if before.is_some() != after.is_some() {
                    return Err(Failure::SourceConflict);
                }
            }
        }
        let changes = diff(&base.config, &candidate);
        let candidate = Arc::new(candidate);
        let mut staged = self.staged.lock().await;
        if request.dry_run {
            let generation = uuid::Uuid::new_v4().to_string();
            *staged = Some(Staged {
                generation: generation.clone(),
                base: base.generation.clone(),
                config: candidate,
                created: Instant::now(),
            });
            return Ok(ReloadResult {
                scope: "file-providers",
                status: "planned",
                current_generation: base.generation.clone(),
                candidate_generation: generation,
                resulting_generation: base.generation,
                changes,
            });
        }
        let plan = staged.as_ref().ok_or(Failure::CandidateConflict)?;
        if request.expected_candidate_generation.as_ref() != Some(&plan.generation)
            || plan.base != base.generation
            || plan.created.elapsed() > Duration::from_secs(60)
            || plan.config.as_ref() != candidate.as_ref()
        {
            return Err(Failure::CandidateConflict);
        }
        let generation = plan.generation.clone();
        let resulting_generation = if changes.is_empty() {
            base.generation.clone()
        } else {
            generation.clone()
        };
        if !changes.is_empty() {
            let default_provider_hint = candidate
                .resolve_provider("default")
                .map(|p| p.resolved_name)
                .or_else(|| {
                    base.default_provider_hint
                        .filter(|name| candidate.resolve_provider(name).is_some())
                });
            self.head.send_replace(Snapshot {
                generation: resulting_generation.clone(),
                config: candidate,
                default_provider_hint,
            });
        }
        *staged = None;
        Ok(ReloadResult {
            scope: "file-providers",
            status: if changes.is_empty() {
                "unchanged"
            } else {
                "applied"
            },
            current_generation: base.generation,
            candidate_generation: generation,
            resulting_generation,
            changes,
        })
    }
}

fn diff(before: &AppConfig, after: &AppConfig) -> Vec<Change> {
    let mut names: Vec<_> = before
        .providers
        .keys()
        .chain(after.providers.keys())
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|name| {
            let a = before.providers.get(&name);
            let b = after.providers.get(&name);
            if a == b {
                return None;
            }
            Some(Change {
                provider: name,
                action: if a.is_none() {
                    "added"
                } else if b.is_none() {
                    "removed"
                } else {
                    "updated"
                },
            })
        })
        .collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn fixed(config: &AppConfig) -> AppConfig {
    let mut config = config.clone();
    config.providers.clear();
    config.upstream.clear();
    config
}
fn validate(before: &AppConfig, after: &AppConfig) -> Result<(), Failure> {
    if fixed(before) != fixed(after) {
        return Err(Failure::RestartRequired);
    }
    if before.providers.keys().any(|name| !valid_name(name)) {
        return Err(Failure::InvalidProvider);
    }
    let mut names = HashSet::new();
    for (name, provider) in &after.providers {
        if !valid_name(name) || !names.insert(name.to_ascii_lowercase()) {
            return Err(Failure::InvalidProvider);
        }
        let kind = ProviderType::try_from_str(&provider.type_).ok_or(Failure::InvalidProvider)?;
        if provider
            .compat
            .max_tokens_field
            .as_deref()
            .is_some_and(|field| !matches!(field, "max_tokens" | "max_completion_tokens"))
        {
            return Err(Failure::InvalidProvider);
        }
        for (name, value) in &provider.headers {
            if http::HeaderName::from_bytes(name.as_bytes()).is_err()
                || http::HeaderValue::from_str(value).is_err()
            {
                return Err(Failure::InvalidProvider);
            }
        }
        let mut models = HashSet::new();
        let old = before.providers.get(name);
        for model in &provider.models {
            if model.id.is_empty()
                || model.id.len() > 512
                || model.id.chars().any(char::is_control)
                || !models.insert(&model.id)
            {
                return Err(Failure::InvalidProvider);
            }
            let costs = [
                model.cost.input,
                model.cost.output,
                model.cost.cache_read,
                model.cost.cache_write,
            ];
            if costs.iter().any(|v| !v.is_finite() || *v < 0.0) {
                return Err(Failure::PricingChange);
            }
            let previous = old.and_then(|p| p.models.iter().find(|m| m.id == model.id));
            if previous.is_some_and(|m| m.cost != model.cost)
                || (previous.is_none() && costs.iter().any(|v| *v != 0.0))
            {
                return Err(Failure::PricingChange);
            }
        }
        // Unchanged reference-based destinations need no new activation. New/changed
        // destinations must be literal/default URLs: planning never opens the keychain.
        if old.is_none_or(|p| p.base_url != provider.base_url || p.type_ != provider.type_) {
            let base_url = if provider.base_url.is_empty() {
                kind.info().default_base_url
            } else {
                Some(provider.base_url.as_str())
            };
            if kind != ProviderType::Mock {
                let address = base_url.ok_or(Failure::InvalidDestination)?;
                let url = url::Url::parse(address).map_err(|_| Failure::InvalidDestination)?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || crate::network_acl::check_url_allowed(&after.network, address).is_err()
                {
                    return Err(Failure::InvalidDestination);
                }
            }
        }
    }
    for (name, provider) in &before.providers {
        if matches!(
            provider.provider_type(),
            ProviderType::Fal | ProviderType::Replicate
        ) && after.providers.get(name) != Some(provider)
        {
            return Err(Failure::NativeJobChange);
        }
    }
    Ok(())
}

/// Validate unsupported input keys before typed deserialization can ignore them.
/// Operational EAVS_* environment keys are not routing declarations.
pub fn validate_input(input: &serde_json::Value) -> Result<(), Failure> {
    let root = input.as_object().ok_or(Failure::InvalidConfiguration)?;
    let allowed = [
        "server",
        "inference",
        "providers",
        "upstream",
        "logging",
        "analysis",
        "policy",
        "delegated_fetch",
        "state",
        "keys",
        "capture",
        "transform",
        "network",
        "egress",
        "mock_responses",
        "native_pricing",
        "auth_token",
        "fleet",
        "program_status",
        "pools",
    ];
    if root.keys().any(|key| {
        !allowed.contains(&key.as_str())
            && std::env::var_os(format!("EAVS_{}", key.to_ascii_uppercase())).is_none()
    }) {
        return Err(Failure::InvalidConfiguration);
    }
    let schema = serde_json::to_value(ProviderConfig::default())
        .map_err(|_| Failure::InvalidConfiguration)?;
    let fields = schema.as_object().ok_or(Failure::InvalidConfiguration)?;
    for table in ["providers", "upstream"] {
        if let Some(providers) = root.get(table) {
            for provider in providers
                .as_object()
                .ok_or(Failure::InvalidConfiguration)?
                .values()
            {
                let provider = provider.as_object().ok_or(Failure::InvalidConfiguration)?;
                if provider.keys().any(|key| !fields.contains_key(key)) {
                    return Err(Failure::InvalidConfiguration);
                }
                if let Some(compat) = provider.get("compat") {
                    let schema = serde_json::to_value(crate::provider::CompatSettings::default())
                        .map_err(|_| Failure::InvalidConfiguration)?;
                    check_keys(compat, &schema)?;
                }
                if let Some(models) = provider.get("models") {
                    let model: crate::config::ModelShortlistEntry = serde_json::from_value(
                        serde_json::json!({"id":"schema","compat":{"schema":false}}),
                    )
                    .map_err(|_| Failure::InvalidConfiguration)?;
                    let schema =
                        serde_json::to_value(model).map_err(|_| Failure::InvalidConfiguration)?;
                    for model in models.as_array().ok_or(Failure::InvalidConfiguration)? {
                        check_keys(model, &schema)?;
                        if let Some(cost) = model.get("cost") {
                            let schema = serde_json::to_value(crate::config::ModelCost::default())
                                .map_err(|_| Failure::InvalidConfiguration)?;
                            check_keys(cost, &schema)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn check_keys(input: &serde_json::Value, schema: &serde_json::Value) -> Result<(), Failure> {
    let input = input.as_object().ok_or(Failure::InvalidConfiguration)?;
    let schema = schema.as_object().ok_or(Failure::InvalidConfiguration)?;
    if input.keys().any(|key| !schema.contains_key(key)) {
        return Err(Failure::InvalidConfiguration);
    }
    Ok(())
}

#[cfg(test)]
mod file_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod tests;
