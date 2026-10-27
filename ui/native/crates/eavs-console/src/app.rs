//! The console window: instances on the left, the selected instance's
//! providers, models and subscription usage on the right.
//!
//! Every action is one operator-API call made on a background executor
//! (ADR-0022); the view only shows what the gateway reports and refreshes
//! after each change. Nothing here holds or displays upstream credentials.

use std::sync::Arc;
use std::time::Duration;

use eavs_console_core::client::{AccountUsage, ApiError, Availability, Client, ProviderSource};
use eavs_console_core::presets::{Auth, PRESETS, Preset, unique_name};
use eavs_console_core::{Instance, NewProvider, Origin, ProbeResult};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Sizable as _, WindowExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Window};

use crate::config::Config;

/// The console's sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Overview,
    Providers,
    Models,
    Usage,
}

impl Section {
    pub const ALL: [Self; 4] = [Self::Overview, Self::Providers, Self::Models, Self::Usage];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Providers => "Providers",
            Self::Models => "Models",
            Self::Usage => "Usage",
        }
    }
}

/// Connection state of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Ready,
    Failed(String),
}

/// Everything the console knows about one instance.
pub struct InstanceState {
    pub instance: Instance,
    pub client: Option<Arc<Client>>,
    pub conn: Conn,
    pub sources: Vec<ProviderSource>,
    pub availability: Availability,
    /// `Err` holds why usage is unavailable (e.g. an older gateway).
    pub usage: Result<Vec<AccountUsage>, String>,
    pub loaded_at: Option<chrono::DateTime<chrono::Utc>>,
    pub loading: bool,
}

impl InstanceState {
    pub fn new(instance: Instance) -> Self {
        Self {
            instance,
            client: None,
            conn: Conn::Connecting,
            sources: Vec::new(),
            availability: Availability::default(),
            usage: Ok(Vec::new()),
            loaded_at: None,
            loading: false,
        }
    }
}

/// One fetch of an instance's state.
struct Snapshot {
    sources: Result<Vec<ProviderSource>, ApiError>,
    availability: Result<Availability, ApiError>,
    usage: Result<Vec<AccountUsage>, ApiError>,
}

/// The add-provider flow: pick a preset, then fill the form.
pub struct AddFlow {
    pub preset: Option<&'static Preset>,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct App {
    pub instances: Vec<InstanceState>,
    pub selected: usize,
    pub section: Section,
    /// Provider whose detail is open.
    pub expanded: Option<String>,
    pub add: Option<AddFlow>,
    /// Last provider test result, by provider.
    pub test_result: Option<(String, Result<ProbeResult, String>)>,
    pub testing: Option<String>,
    pub config_errors: Vec<String>,
    refresh_every: Duration,
    // Inputs.
    pub name_input: Entity<InputState>,
    pub url_input: Entity<InputState>,
    pub key_input: Entity<InputState>,
    pub models_input: Entity<InputState>,
    pub add_model_input: Entity<InputState>,
    pub test_model_input: Entity<InputState>,
    pub rotate_input: Entity<InputState>,
}

impl App {
    pub fn new(config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut instances = Vec::new();
        let mut config_errors = Vec::new();
        if let Some(local) = config
            .local_instance
            .then(eavs_console_core::instances::eavs_config_path)
            .flatten()
            .and_then(|path| eavs_console_core::instances::discover_local(&path))
        {
            instances.push(InstanceState::new(local));
        }
        for declared in config.instances {
            let name = declared.name.clone();
            match declared.into_instance() {
                Ok(instance) => instances.push(InstanceState::new(instance)),
                Err(error) => config_errors.push(format!("{name}: {error:#}")),
            }
        }
        let mut app = Self::build(
            instances,
            config_errors,
            Duration::from_secs(config.refresh_secs.max(5)),
            window,
            cx,
        );
        for index in 0..app.instances.len() {
            app.connect(index, cx);
        }
        app.start_refresh_loop(cx);
        app
    }

    /// The app without network activity: inputs and state only.
    pub fn build(
        instances: Vec<InstanceState>,
        config_errors: Vec<String>,
        refresh_every: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let key_input = input("API key", window, cx);
        let rotate_input = input("new API key", window, cx);
        for masked in [&key_input, &rotate_input] {
            masked.update(cx, |state, cx| state.set_masked(true, window, cx));
        }
        Self {
            instances,
            selected: 0,
            section: Section::Overview,
            expanded: None,
            add: None,
            test_result: None,
            testing: None,
            config_errors,
            refresh_every,
            name_input: input("provider name, e.g. rtx6000", window, cx),
            url_input: input("https://… or http://127.0.0.1:8080/v1", window, cx),
            key_input,
            models_input: input(
                "models, comma separated (empty: serve the full catalog)",
                window,
                cx,
            ),
            add_model_input: input("model id, e.g. qwen3.8", window, cx),
            test_model_input: input("model to test", window, cx),
            rotate_input,
        }
    }

    pub fn current(&self) -> Option<&InstanceState> {
        self.instances.get(self.selected)
    }

    fn client(&self) -> Option<Arc<Client>> {
        self.current().and_then(|state| state.client.clone())
    }

    /// Resolve credentials (may touch the keychain) off the UI thread.
    fn connect(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(state) = self.instances.get_mut(index) else {
            return;
        };
        state.conn = Conn::Connecting;
        let instance = state.instance.clone();
        cx.spawn(async move |this, cx| {
            let client = cx
                .background_spawn(async move { Client::connect(&instance) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.instances.get_mut(index) {
                    match client {
                        Ok(client) => {
                            state.client = Some(Arc::new(client));
                            this.load(index, false, cx);
                        }
                        Err(error) => state.conn = Conn::Failed(error.to_string()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch providers, availability and usage of one instance.
    pub fn load(&mut self, index: usize, refresh_usage: bool, cx: &mut Context<Self>) {
        let Some(state) = self.instances.get_mut(index) else {
            return;
        };
        let Some(client) = state.client.clone() else {
            return;
        };
        if state.loading {
            return;
        }
        state.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_spawn(async move {
                    Snapshot {
                        sources: client.sources(),
                        availability: client.availability(),
                        usage: client.usage(refresh_usage),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.instances.get_mut(index) {
                    state.loading = false;
                    match snapshot.sources {
                        Ok(sources) => {
                            state.sources = sources;
                            state.conn = Conn::Ready;
                            state.loaded_at = Some(chrono::Utc::now());
                        }
                        Err(error) => state.conn = Conn::Failed(error.to_string()),
                    }
                    if let Ok(availability) = snapshot.availability {
                        state.availability = availability;
                    }
                    state.usage = snapshot.usage.map_err(|error| match error {
                        ApiError::Rejected { status: 404, .. } => {
                            "This gateway does not report subscription usage yet; update eavs."
                                .to_owned()
                        }
                        other => other.to_string(),
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_refresh_loop(&mut self, cx: &mut Context<Self>) {
        let every = self.refresh_every;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let alive = this.update(cx, |this, cx| {
                    let index = this.selected;
                    this.load(index, false, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn select_instance(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.expanded = None;
        self.add = None;
        self.test_result = None;
        if let Some(state) = self.instances.get(index) {
            if state.client.is_none() {
                self.connect(index, cx);
            } else {
                self.load(index, false, cx);
            }
        }
        cx.notify();
    }

    pub fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let index = self.selected;
        if self
            .instances
            .get(index)
            .is_some_and(|s| s.client.is_none())
        {
            self.connect(index, cx);
        } else {
            self.load(index, self.section == Section::Usage, cx);
        }
    }

    /// Run one mutation, report the outcome, then reload.
    fn mutate<F>(&mut self, label: String, op: F, window: &mut Window, cx: &mut Context<Self>)
    where
        F: FnOnce(&Client) -> Result<(), ApiError> + Send + 'static,
    {
        let Some(client) = self.client() else {
            window.push_notification("Not connected to this gateway", cx);
            return;
        };
        let index = self.selected;
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { op(&client) }).await;
            let message = match &result {
                Ok(()) => label,
                Err(error) => format!("{label} failed: {error}"),
            };
            let _ = window_handle.update(cx, |_, window, cx| window.push_notification(message, cx));
            let _ = this.update(cx, |this, cx| this.load(index, false, cx));
        })
        .detach();
    }

    // ----- Providers -------------------------------------------------------

    pub fn toggle_provider(&mut self, name: &str, cx: &mut Context<Self>) {
        self.expanded = if self.expanded.as_deref() == Some(name) {
            None
        } else {
            Some(name.to_owned())
        };
        self.test_result = None;
        cx.notify();
    }

    pub fn open_add(&mut self, cx: &mut Context<Self>) {
        self.add = Some(AddFlow {
            preset: None,
            error: None,
            busy: false,
        });
        self.section = Section::Providers;
        cx.notify();
    }

    pub fn close_add(&mut self, cx: &mut Context<Self>) {
        self.add = None;
        cx.notify();
    }

    pub fn choose_preset(
        &mut self,
        preset: &'static Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let taken: Vec<String> = self
            .current()
            .map(|s| s.sources.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        let name = unique_name(preset.name, &taken);
        self.name_input
            .update(cx, |s, cx| s.set_value(name, window, cx));
        self.url_input
            .update(cx, |s, cx| s.set_value(preset.base_url, window, cx));
        self.key_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.models_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        if let Some(add) = &mut self.add {
            add.preset = Some(preset);
            add.error = None;
        }
        cx.notify();
    }

    pub fn create_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preset) = self.add.as_ref().and_then(|a| a.preset) else {
            return;
        };
        let models = self
            .models_input
            .read(cx)
            .value()
            .split([',', '\n'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let provider = NewProvider {
            name: self.name_input.read(cx).value().trim().to_owned(),
            provider_type: preset.provider_type.to_owned(),
            base_url: self.url_input.read(cx).value().trim().to_owned(),
            api_key: self.key_input.read(cx).value().trim().to_owned(),
            models,
            probe: preset.probe,
        };
        if let Err(message) = provider.validate() {
            if let Some(add) = &mut self.add {
                add.error = Some(message);
            }
            cx.notify();
            return;
        }
        if matches!(preset.auth, Auth::ApiKey { .. }) && provider.api_key.is_empty() {
            if let Some(add) = &mut self.add {
                add.error = Some("Paste the API key".into());
            }
            cx.notify();
            return;
        }
        let Some(client) = self.client() else {
            return;
        };
        if let Some(add) = &mut self.add {
            add.busy = true;
            add.error = None;
        }
        cx.notify();
        let index = self.selected;
        let name = provider.name.clone();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { client.create_provider(&provider) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.add = None;
                        this.expanded = Some(name.clone());
                        this.load(index, false, cx);
                        let _ = window_handle.update(cx, |_, window, cx| {
                            window.push_notification(format!("Added {name}"), cx);
                        });
                    }
                    Err(error) => {
                        if let Some(add) = &mut this.add {
                            add.busy = false;
                            add.error = Some(error.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn delete_provider(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.expanded = None;
        let target = name.clone();
        self.mutate(
            format!("Deleted {name}"),
            move |client| client.delete_provider(&target),
            window,
            cx,
        );
    }

    pub fn add_model(&mut self, provider: String, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.add_model_input.read(cx).value().trim().to_owned();
        if id.is_empty() {
            return;
        }
        self.add_model_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        let model = id.clone();
        self.mutate(
            format!("Added {id} to {provider}"),
            move |client| client.add_model(&provider, &model),
            window,
            cx,
        );
    }

    pub fn remove_model(
        &mut self,
        provider: String,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = format!("Removed {id} from {provider}");
        self.mutate(
            label,
            move |client| client.remove_model(&provider, &id),
            window,
            cx,
        );
    }

    pub fn rotate_key(
        &mut self,
        provider: String,
        revision: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.rotate_input.read(cx).value().trim().to_owned();
        if key.is_empty() {
            return;
        }
        self.rotate_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.mutate(
            format!("Rotated the key of {provider}"),
            move |client| client.rotate_key(&provider, &revision, &key),
            window,
            cx,
        );
    }

    pub fn test_provider(&mut self, provider: String, cx: &mut Context<Self>) {
        let model = self.test_model_input.read(cx).value().trim().to_owned();
        let model = if model.is_empty() {
            self.current()
                .and_then(|s| {
                    s.availability
                        .providers
                        .iter()
                        .find(|p| p.provider == provider)
                        .and_then(|p| p.models.first())
                        .map(|m| m.id.clone())
                })
                .unwrap_or_default()
        } else {
            model
        };
        if model.is_empty() {
            self.test_result = Some((provider, Err("Enter a model to test".into())));
            cx.notify();
            return;
        }
        let Some(client) = self.client() else {
            return;
        };
        self.testing = Some(provider.clone());
        self.test_result = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = provider.clone();
            let result = cx
                .background_spawn(async move { client.test_provider(&target, &model) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.testing = None;
                this.test_result = Some((provider, result.map_err(|e| e.to_string())));
                cx.notify();
            });
        })
        .detach();
    }

    // ----- Models ------------------------------------------------------------

    pub fn set_available(
        &mut self,
        provider: String,
        model: String,
        available: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = if available {
            format!("{model} is available again")
        } else {
            format!("{model} disabled")
        };
        self.mutate(
            label,
            move |client| client.set_available(&provider, &model, available),
            window,
            cx,
        );
    }

    pub fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        let index = self.selected;
        self.load(index, true, cx);
    }
}

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::ui::render(self, window, cx)
    }
}

/// Short origin label for the instance list.
pub const fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::Local => "this machine",
        Origin::Declared => "remote",
    }
}

/// Whether the provider form needs an API key field.
pub const fn needs_key(preset: &Preset) -> bool {
    matches!(preset.auth, Auth::ApiKey { .. })
}

/// All presets, for the picker.
pub fn presets() -> &'static [Preset] {
    PRESETS
}

/// Keep the input element constructor in one place.
pub fn input(state: &Entity<InputState>) -> Input {
    Input::new(state).small().appearance(false)
}
