use crate::fleet::{Change, FleetConfig, ProviderSource};
use byteowlz_tui_kit::prelude::*;
use crossterm::event::{KeyCode, KeyModifiers};

pub struct Machine {
    pub providers: Vec<ProviderSource>,
    pub loaded: bool,
    pub status: String,
    pub result: Option<String>,
}

pub struct App {
    pub fleet: FleetConfig,
    pub machines: Vec<Machine>,
    pub instances: Selection,
    pub providers: Selection,
    pub provider_focus: bool,
    pub overlay: Option<Overlay>,
    pub palette: Option<CommandPalette>,
    pub busy: bool,
    pub status: String,
    pub preview_scroll: u16,
}

pub enum Overlay {
    Form(Form),
    Preview(Vec<Target>),
}

pub struct Form {
    pub operation: Operation,
    pub fields: Vec<(&'static str, String)>,
    pub cursor: usize,
}

#[derive(Clone, Copy)]
pub enum Operation {
    Add,
    Rotate,
    Rollback,
}

pub struct Target {
    pub index: usize,
    pub description: String,
    pub change: Option<Change>,
}

pub enum Effect {
    None,
    Refresh,
    Apply(Vec<Target>),
    Quit,
}

pub fn actions() -> Vec<Action> {
    [
        ("refresh", "Refresh inventories", 'r'),
        ("add", "Add provider", 'a'),
        ("rotate", "Rotate provider credential", 'c'),
        ("rollback", "Rollback credential", 'u'),
        ("quit", "Quit", 'q'),
    ]
    .into_iter()
    .map(|(id, label, key)| Action::new(ActionId::new(id), label).key(Key::char(key)))
    .collect()
}

impl App {
    pub fn new(fleet: FleetConfig) -> Self {
        let machines = fleet
            .instances
            .iter()
            .map(|_| Machine {
                providers: Vec::new(),
                loaded: false,
                status: "Not loaded — r refresh".into(),
                result: None,
            })
            .collect();
        Self {
            fleet,
            machines,
            instances: Selection::default(),
            providers: Selection::default(),
            provider_focus: false,
            overlay: None,
            palette: None,
            busy: false,
            status: "Space selects instances; otherwise actions target the focused instance".into(),
            preview_scroll: 0,
        }
    }

    pub fn provider(&self) -> Option<&ProviderSource> {
        self.machines
            .get(self.instances.index())?
            .providers
            .get(self.providers.index())
    }

    pub fn targets(&self) -> Vec<usize> {
        if self.instances.has_selection() {
            self.instances.selected()
        } else {
            vec![self.instances.index()]
        }
    }

    pub fn action(&mut self, id: &str) -> Effect {
        if self.busy {
            self.status = "Waiting for bounded requests to settle; no duplicate writes".into();
            return Effect::None;
        }
        match id {
            "quit" => Effect::Quit,
            "refresh" => Effect::Refresh,
            "add" => {
                self.overlay = Some(Overlay::Form(Form {
                    operation: Operation::Add,
                    cursor: 0,
                    fields: vec![
                        ("Provider name", String::new()),
                        ("Provider type", "openai-compatible".into()),
                        ("Upstream base URL", String::new()),
                        ("Local key ENV name (not key)", String::new()),
                    ],
                }));
                Effect::None
            }
            "rotate" | "rollback" => {
                let Some(provider) = self.provider() else {
                    self.status = "Select a loaded provider first".into();
                    return Effect::None;
                };
                let name = provider.name.clone();
                if id == "rollback" {
                    self.preview_scroll = 0;
                    self.overlay =
                        Some(Overlay::Preview(self.plan(&name, Operation::Rollback, &[])));
                } else {
                    self.overlay = Some(Overlay::Form(Form {
                        operation: Operation::Rotate,
                        cursor: 0,
                        fields: vec![
                            ("Provider name", name),
                            ("Local key ENV name (not key)", String::new()),
                        ],
                    }));
                }
                Effect::None
            }
            _ => Effect::None,
        }
    }

    pub fn input(&mut self, key: Key, router: &mut KeyRouter<'_>, registry: &[Action]) -> Effect {
        if key == Key::ctrl_char('c') {
            return self.action("quit");
        }
        if let Some(palette) = self.palette.as_mut() {
            return match palette.handle(key) {
                PaletteOutcome::Run(id) => {
                    self.palette = None;
                    self.action(id.as_str())
                }
                PaletteOutcome::Closed => {
                    self.palette = None;
                    Effect::None
                }
                PaletteOutcome::Open => Effect::None,
            };
        }
        if self.overlay.is_some() {
            return self.overlay_input(key);
        }
        if key == Key::char(':') || key == Key::ctrl_char('p') || key == Key::char('?') {
            self.palette = Some(CommandPalette::new(registry.to_vec()));
            return Effect::None;
        }
        if key == Key::tab() {
            self.provider_focus = !self.provider_focus;
            router.reset();
            return Effect::None;
        }
        if key == Key::space() && !self.provider_focus {
            self.instances.toggle();
            return Effect::None;
        }
        if key == Key::ctrl_char('a') && !self.provider_focus {
            self.instances.select_all(self.machines.len());
            return Effect::None;
        }
        if self.navigate(key) {
            router.reset();
            return Effect::None;
        }
        match router.feed(key) {
            Route::Action(id) => self.action(id.as_str()),
            _ => Effect::None,
        }
    }

    fn navigate(&mut self, key: Key) -> bool {
        let count = if self.provider_focus {
            self.machines
                .get(self.instances.index())
                .map_or(0, |machine| machine.providers.len())
        } else {
            self.machines.len()
        };
        let selection = if self.provider_focus {
            &mut self.providers
        } else {
            &mut self.instances
        };
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => selection.next(count),
            KeyCode::Up | KeyCode::Char('k') => selection.previous(count),
            KeyCode::Home | KeyCode::Char('g') => selection.top(),
            KeyCode::End | KeyCode::Char('G') => selection.bottom(count),
            _ => return false,
        }
        if !self.provider_focus {
            self.providers.top();
        }
        true
    }

    fn overlay_input(&mut self, key: Key) -> Effect {
        if key == Key::esc() {
            self.overlay = None;
            return Effect::None;
        }
        let Some(overlay) = self.overlay.take() else {
            return Effect::None;
        };
        match overlay {
            Overlay::Preview(targets) if key == Key::enter() => Effect::Apply(targets),
            Overlay::Preview(targets) => {
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.preview_scroll = self.preview_scroll.saturating_add(1);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.preview_scroll = self.preview_scroll.saturating_sub(1);
                    }
                    KeyCode::PageDown => {
                        self.preview_scroll = self.preview_scroll.saturating_add(8);
                    }
                    KeyCode::PageUp => self.preview_scroll = self.preview_scroll.saturating_sub(8),
                    _ => {}
                }
                self.overlay = Some(Overlay::Preview(targets));
                Effect::None
            }
            Overlay::Form(mut form) => {
                if key == Key::enter() {
                    if form.fields.iter().enumerate().any(|(index, (_, value))| {
                        value.trim().is_empty()
                            && !(matches!(form.operation, Operation::Add) && index == 3)
                    }) {
                        self.status = "Complete every field; Enter previews, Esc cancels".into();
                        self.overlay = Some(Overlay::Form(form));
                        return Effect::None;
                    }
                    let name = form.fields.first().map_or("", |(_, value)| value.as_str());
                    self.preview_scroll = 0;
                    self.overlay = Some(Overlay::Preview(self.plan(
                        name,
                        form.operation,
                        &form.fields,
                    )));
                } else {
                    form.edit(key);
                    self.overlay = Some(Overlay::Form(form));
                }
                Effect::None
            }
        }
    }

    fn plan(&self, name: &str, operation: Operation, fields: &[(&str, String)]) -> Vec<Target> {
        self.targets()
            .into_iter()
            .map(|index| {
                let (mut change, mut description) =
                    self.plan_target(index, name, operation, fields);
                if let Some(planned) = &change {
                    if let Err(error) = crate::fleet::validate_change(planned) {
                        change = None;
                        description = format!("Invalid plan: {error}");
                    }
                }
                Target {
                    index,
                    description,
                    change,
                }
            })
            .collect()
    }

    fn plan_target(
        &self,
        index: usize,
        name: &str,
        operation: Operation,
        fields: &[(&str, String)],
    ) -> (Option<Change>, String) {
        let Some(machine) = self.machines.get(index).filter(|machine| machine.loaded) else {
            return (
                None,
                "Unavailable inventory; skipped (refresh first)".into(),
            );
        };
        let provider = machine
            .providers
            .iter()
            .find(|provider| provider.name == name);
        let field = |index: usize| {
            fields
                .get(index)
                .map_or_else(String::new, |(_, value)| value.clone())
        };
        match operation {
            Operation::Add if provider.is_none() => (
                Some(Change::Add {
                    name: name.into(),
                    provider_type: field(1),
                    base_url: field(2),
                    key_env: field(3),
                }),
                format!(
                    "CREATE {name} · {} · {} · key source {}",
                    field(1),
                    field(2),
                    field(3)
                ),
            ),
            Operation::Add => (None, "Already exists; add never overwrites".into()),
            Operation::Rotate | Operation::Rollback => {
                let Some(revision) = provider
                    .filter(|provider| provider.source == "admin")
                    .and_then(|provider| provider.revision.clone())
                else {
                    return (
                        None,
                        "Not admin-managed or missing; explicit migration required".into(),
                    );
                };
                if matches!(operation, Operation::Rotate) {
                    (
                        Some(Change::Rotate {
                            name: name.into(),
                            revision: revision.clone(),
                            key_env: field(1),
                        }),
                        format!(
                            "ROTATE {name} · revision {revision} · key source {}",
                            field(1)
                        ),
                    )
                } else {
                    (
                        Some(Change::Rollback {
                            name: name.into(),
                            revision: revision.clone(),
                        }),
                        format!("ROLLBACK {name} · revision {revision}"),
                    )
                }
            }
        }
    }
}

impl Form {
    fn edit(&mut self, key: Key) {
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.cursor = (self.cursor + 1) % self.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.cursor = (self.cursor + self.fields.len() - 1) % self.fields.len();
            }
            KeyCode::Backspace => {
                if let Some((_, value)) = self.fields.get_mut(self.cursor) {
                    value.pop();
                }
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !character.is_control() =>
            {
                if let Some((label, value)) = self.fields.get_mut(self.cursor) {
                    if value.len() < 2048
                        && (!label.contains("ENV")
                            || character.is_ascii_alphanumeric()
                            || character == '_')
                    {
                        value.push(character);
                    }
                }
            }
            _ => {}
        }
    }
}
