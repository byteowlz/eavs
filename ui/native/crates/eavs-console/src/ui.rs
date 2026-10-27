//! Rendering of the console. Colors come from the role layer (`cx.theme()`),
//! never a hex value; layout follows the template's sunken-sidebar shell.

use eavs_console_core::client::{AccountUsage, ProviderSource};
use eavs_console_core::presets::{Auth, Preset};
use eavs_console_core::view;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, ThemeColor};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, IntoElement, SharedString, Window, div, px,
};

use crate::app::{App, Conn, InstanceState, Section, input, needs_key, origin_label, presets};

pub fn render(app: &mut App, window: &mut Window, cx: &mut Context<App>) -> AnyElement {
    let theme = cx.theme().colors;
    div()
        .size_full()
        .flex()
        .bg(theme.background)
        .text_color(theme.foreground)
        .text_size(px(13.))
        .child(sidebar(app, &theme, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(header(app, &theme, cx))
                .child(
                    div()
                        .id("content")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_6()
                        .child(content(app, &theme, window, cx)),
                ),
        )
        .into_any_element()
}

// ----- Shell ----------------------------------------------------------------

fn sidebar(app: &App, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let mut instances = div().flex().flex_col().gap_1();
    for (index, state) in app.instances.iter().enumerate() {
        let selected = index == app.selected;
        let dot = conn_color(&state.conn, theme);
        instances = instances.child(
            div()
                .id(SharedString::from(format!("instance-{index}")))
                .px_2()
                .py_2()
                .rounded(px(6.))
                .cursor_pointer()
                .when_else(selected, |d| d.bg(theme.sidebar_accent), |d| d)
                .hover(|d| d.bg(theme.sidebar_accent))
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.select_instance(index, cx)),
                )
                .flex()
                .items_center()
                .gap_2()
                .child(div().size(px(8.)).rounded_full().bg(dot))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(state.instance.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(format!(
                                    "{} · {}",
                                    origin_label(state.instance.origin),
                                    host(&state.instance.url)
                                )),
                        ),
                ),
        );
    }
    if app.instances.is_empty() {
        instances = instances.child(
            div()
                .text_color(theme.muted_foreground)
                .text_size(px(12.))
                .child(
                    "No gateway found. Add [[instances]] to ~/.config/eavs-console/config.toml.",
                ),
        );
    }

    let mut nav = div().flex().flex_col().gap_1();
    for section in Section::ALL {
        let active = section == app.section;
        nav =
            nav.child(
                div()
                    .id(SharedString::from(format!("nav-{}", section.label())))
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(if active {
                        theme.sidebar_primary
                    } else {
                        theme.sidebar_foreground
                    })
                    .when_else(
                        active,
                        |d| d.bg(theme.sidebar_accent).font_weight(FontWeight::SEMIBOLD),
                        |d| d,
                    )
                    .hover(|d| d.bg(theme.sidebar_accent))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.select_section(section, cx)
                    }))
                    .child(section.label()),
            );
    }

    div()
        .w(px(232.))
        .h_full()
        .flex()
        .flex_col()
        .gap_5()
        .p_3()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            div()
                .px_2()
                .pt_1()
                .flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .text_size(px(17.))
                        .font_weight(FontWeight::BOLD)
                        .font_family(crate::theme::accent_font(cx))
                        .text_color(theme.primary)
                        .child("eavs"),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .text_size(px(11.))
                        .child("console"),
                ),
        )
        .child(label("Gateways", theme))
        .child(instances)
        .child(label("Manage", theme))
        .child(nav)
        .child(div().flex_1())
        .child(
            div().pb_2().child(
                Button::new("add-provider")
                    .primary()
                    .small()
                    .label("Add provider")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_add(cx))),
            ),
        )
}

fn header(app: &App, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let (title, subtitle) = app.current().map_or_else(
        || ("No gateway".to_owned(), String::new()),
        |state| {
            let status = match &state.conn {
                Conn::Connecting => "connecting…".to_owned(),
                Conn::Ready => format!(
                    "{} · updated {}",
                    state.instance.url,
                    view::ago(state.loaded_at, chrono::Utc::now())
                ),
                Conn::Failed(error) => error.clone(),
            };
            (state.instance.name.clone(), status)
        },
    );
    let failed = app
        .current()
        .is_some_and(|s| matches!(s.conn, Conn::Failed(_)));
    let loading = app.current().is_some_and(|s| s.loading);
    div()
        .h(px(56.))
        .px_6()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .truncate()
                        .text_color(if failed {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(subtitle),
                ),
        )
        .child(div().flex_1())
        .child(
            Button::new("reload")
                .outline()
                .small()
                .loading(loading)
                .label("Refresh")
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.reload(cx))),
        )
}

fn content(
    app: &mut App,
    theme: &ThemeColor,
    window: &mut Window,
    cx: &mut Context<App>,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_4().max_w(px(980.));
    for error in &app.config_errors {
        column = column.child(notice(theme.warning, &format!("Config: {error}"), theme));
    }
    if app.add.is_some() {
        return column
            .child(add_provider(app, theme, window, cx))
            .into_any_element();
    }
    let Some(state) = app.current() else {
        return column
            .child(empty(
                "No gateway to manage",
                "Run eavs on this machine, or declare a remote gateway in ~/.config/eavs-console/config.toml.",
                theme,
            ))
            .into_any_element();
    };
    if let Conn::Failed(error) = &state.conn {
        column = column.child(notice(theme.danger, error, theme));
    }
    let body = match app.section {
        Section::Overview => overview(state, theme, cx).into_any_element(),
        Section::Providers => providers(app, theme, cx).into_any_element(),
        Section::Models => models(state, theme, cx).into_any_element(),
        Section::Usage => usage(state, theme, cx).into_any_element(),
    };
    column.child(body).into_any_element()
}

// ----- Overview -------------------------------------------------------------

fn overview(state: &InstanceState, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let providers = state.sources.len();
    let models: usize = state
        .availability
        .providers
        .iter()
        .map(|p| p.models.len())
        .sum();
    let unavailable: usize = state
        .availability
        .availability
        .iter()
        .map(|p| p.models.len())
        .sum();
    let probe_errors: Vec<String> = state
        .availability
        .probe
        .iter()
        .filter(|(_, probe)| !probe.ok)
        .map(|(name, probe)| format!("{name}: {}", probe.error.clone().unwrap_or_default()))
        .collect();
    let accounts = state.usage.as_ref().map(Vec::len).unwrap_or(0);

    let stats = div()
        .flex()
        .gap_3()
        .child(stat("Providers", &providers.to_string(), theme))
        .child(stat("Models", &models.to_string(), theme))
        .child(stat("Unavailable", &unavailable.to_string(), theme))
        .child(stat("Accounts", &accounts.to_string(), theme));

    let mut column = div().flex().flex_col().gap_4().child(stats);
    for error in probe_errors {
        column = column.child(notice(
            theme.warning,
            &format!("Local runtime not answering — {error}"),
            theme,
        ));
    }
    // The fullest meter of every account: what runs out first.
    if let Ok(accounts) = &state.usage {
        let mut rows = div().flex().flex_col().gap_3();
        let mut any = false;
        for account in accounts {
            let fullest = account
                .meters
                .iter()
                .filter(|m| view::meter_fraction(m).is_some())
                .max_by(|a, b| {
                    view::meter_fraction(a)
                        .partial_cmp(&view::meter_fraction(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            if let Some(meter) = fullest {
                any = true;
                rows = rows.child(meter_row(
                    &format!("{} · {}", account.provider, meter.label),
                    meter,
                    theme,
                ));
            }
        }
        if any {
            column = column.child(
                card(theme)
                    .child(section_title("Closest to a limit", theme))
                    .child(rows),
            );
        }
    }
    if providers == 0 && matches!(state.conn, Conn::Ready) {
        column = column.child(
            card(theme)
                .child(section_title("Connect your first provider", theme))
                .child(div().text_color(theme.muted_foreground).child(
                    "Add a local runtime, a subscription or an API key. Credentials stay on this gateway.",
                ))
                .child(
                    Button::new("first-provider")
                        .primary()
                        .small()
                        .label("Add provider")
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_add(cx))),
                ),
        );
    }
    column
}

fn stat(name: &str, value: &str, theme: &ThemeColor) -> impl IntoElement + use<> {
    card(theme)
        .flex_1()
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child(name.to_owned()),
        )
        .child(
            div()
                .text_size(px(22.))
                .font_weight(FontWeight::BOLD)
                .child(value.to_owned()),
        )
}

// ----- Providers ------------------------------------------------------------

fn providers(app: &App, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let Some(state) = app.current() else {
        return div();
    };
    let mut list = div().flex().flex_col().gap_3();
    let mut sources = state.sources.clone();
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    for source in sources {
        let open = app.expanded.as_deref() == Some(source.name.as_str());
        list = list.child(provider_card(app, state, &source, open, theme, cx));
    }
    if state.sources.is_empty() {
        list = list.child(empty(
            "No providers yet",
            "Add a local runtime, a subscription login or an API key.",
            theme,
        ));
    }
    list
}

fn provider_card(
    app: &App,
    state: &InstanceState,
    source: &ProviderSource,
    open: bool,
    theme: &ThemeColor,
    cx: &mut Context<App>,
) -> impl IntoElement {
    let name = source.name.clone();
    let models = state
        .availability
        .providers
        .iter()
        .find(|p| p.provider == source.name)
        .map(|p| p.models.clone())
        .unwrap_or_default();
    let probe = state.availability.probe.get(&source.name);
    let summary = if models.is_empty() {
        "full catalog".to_owned()
    } else {
        format!(
            "{} model{}",
            models.len(),
            if models.len() == 1 { "" } else { "s" }
        )
    };
    let toggle_name = name.clone();
    let mut card = card(theme).child(
        div()
            .id(SharedString::from(format!("provider-{name}")))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_provider(&toggle_name, cx)
            }))
            .child(div().font_weight(FontWeight::SEMIBOLD).child(name.clone()))
            .child(badge(&source.provider_type, theme.muted_foreground))
            .child(if source.managed() {
                badge("managed here", theme.success)
            } else {
                badge("config file", theme.warning)
            })
            .when_some(probe, |d, probe| {
                d.child(if probe.ok {
                    badge(&format!("serving {}", probe.served.len()), theme.info)
                } else {
                    badge("runtime down", theme.danger)
                })
            })
            .when(!source.enabled, |d| {
                d.child(badge("disabled", theme.danger))
            })
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).child(summary))
            .child(div().text_color(theme.muted_foreground).child(if open {
                "▾"
            } else {
                "▸"
            })),
    );
    if !open {
        return card;
    }

    let managed = source.managed();
    if !managed {
        card = card.child(div().text_color(theme.muted_foreground).text_size(px(12.)).child(
            "Declared in the gateway's config file: edit it there (or migrate it). Model availability still works here.",
        ));
    }
    let mut table = div().flex().flex_col();
    for model in &models {
        let unavailable = state.availability.deactivation(&source.name, &model.id);
        let (provider, id) = (name.clone(), model.id.clone());
        table = table.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().child(model.id.clone()))
                .when_some(unavailable, |d, why| {
                    d.child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.warning)
                            .child(format!(
                                "unavailable · {}",
                                why.reason.clone().unwrap_or_else(|| why.set_by.clone())
                            )),
                    )
                })
                .when(managed, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("rm-{provider}-{id}")))
                            .ghost()
                            .xsmall()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.remove_model(provider.clone(), id.clone(), window, cx);
                            })),
                    )
                }),
        );
    }
    card = card.child(table);

    if managed {
        let add_target = name.clone();
        let rotate_target = name.clone();
        let revision = source.revision.clone().unwrap_or_default();
        card = card
            .child(
                row()
                    .child(boxed(input(&app.add_model_input), theme).flex_1())
                    .child(
                        Button::new("add-model")
                            .outline()
                            .small()
                            .label("Add model")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.add_model(add_target.clone(), window, cx);
                            })),
                    ),
            )
            .child(
                row()
                    .child(boxed(input(&app.rotate_input).mask_toggle(), theme).flex_1())
                    .child(
                        Button::new("rotate-key")
                            .outline()
                            .small()
                            .label("Rotate key")
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.rotate_key(
                                    rotate_target.clone(),
                                    revision.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    ),
            );
    }

    let test_target = name.clone();
    let delete_target = name.clone();
    let testing = app.testing.as_deref() == Some(name.as_str());
    card = card.child(
        row()
            .child(boxed(input(&app.test_model_input), theme).flex_1())
            .child(
                Button::new("test-provider")
                    .outline()
                    .small()
                    .loading(testing)
                    .label("Test")
                    .tooltip("Sends one small request (uses a few tokens)")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.test_provider(test_target.clone(), cx);
                    })),
            )
            .when(managed, |d| {
                d.child(
                    Button::new("delete-provider")
                        .danger()
                        .small()
                        .label("Delete")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.delete_provider(delete_target.clone(), window, cx);
                        })),
                )
            }),
    );
    if let Some((provider, result)) = &app.test_result
        && *provider == name
    {
        card = card.child(match result {
            Ok(probe) => {
                let mut lines = div().flex().flex_col().gap_1().child(
                    div()
                        .text_color(if probe.ok {
                            theme.success
                        } else {
                            theme.danger
                        })
                        .child(if probe.ok { "Answered" } else { "Failed" }),
                );
                for stage in &probe.stages {
                    lines = lines.child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "{}: {}{} {}",
                                stage.name,
                                stage.status,
                                stage
                                    .latency_ms
                                    .map(|ms| format!(" ({ms} ms)"))
                                    .unwrap_or_default(),
                                stage.detail
                            )),
                    );
                }
                lines.into_any_element()
            }
            Err(error) => div()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element(),
        });
    }
    card
}

// ----- Add provider ---------------------------------------------------------

fn add_provider(
    app: &App,
    theme: &ThemeColor,
    _window: &mut Window,
    cx: &mut Context<App>,
) -> impl IntoElement {
    let Some(flow) = &app.add else {
        return div();
    };
    let instance = app
        .current()
        .map(|s| s.instance.name.clone())
        .unwrap_or_default();
    let mut column = div().flex().flex_col().gap_4().child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::BOLD)
                    .child("Add provider"),
            )
            .child(badge(&format!("on {instance}"), theme.info))
            .child(div().flex_1())
            .child(
                Button::new("cancel-add")
                    .ghost()
                    .small()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_add(cx))),
            ),
    );

    // Preset tiles.
    let mut grid = div().flex().flex_wrap().gap_2();
    for (index, preset) in presets().iter().enumerate() {
        let selected = flow.preset.is_some_and(|p| p.id == preset.id);
        grid = grid.child(
            div()
                .id(SharedString::from(format!("preset-{index}")))
                .w(px(220.))
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(if selected {
                    theme.primary
                } else {
                    theme.border
                })
                .bg(if selected {
                    theme.primary.alpha(0.08)
                } else {
                    theme.popover
                })
                .cursor_pointer()
                .hover(|d| d.border_color(theme.primary))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.choose_preset(&presets()[index], window, cx);
                }))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(preset.label))
                        .when(preset.usage, |d| d.child(badge("usage meters", theme.info))),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme.muted_foreground)
                        .child(preset.blurb),
                ),
        );
    }
    column = column.child(grid);

    if let Some(preset) = flow.preset {
        column = column.child(provider_form(
            app,
            preset,
            flow.busy,
            flow.error.as_deref(),
            theme,
            cx,
        ));
    }
    column
}

fn provider_form(
    app: &App,
    preset: &'static Preset,
    busy: bool,
    error: Option<&str>,
    theme: &ThemeColor,
    cx: &mut Context<App>,
) -> impl IntoElement {
    let mut form = card(theme).child(section_title(preset.label, theme));
    if let Auth::Subscription { login } = preset.auth {
        return form
            .child(div().text_color(theme.muted_foreground).child(format!(
                "Subscriptions are held by the gateway that serves them, so its login never leaves that machine. \
                 On the gateway, run: eavs login {login}"
            )))
            .child(div().text_color(theme.muted_foreground).child(
                "Then add a key bound to that login (eavs keys create --oauth-user default). \
                 Usage meters appear under Usage once the login is in place.",
            ));
    }
    form = form
        .child(field("Name", boxed(input(&app.name_input), theme), theme))
        .child(field(
            "Base URL",
            boxed(input(&app.url_input), theme),
            theme,
        ));
    if needs_key(preset) {
        let hint = match preset.auth {
            Auth::ApiKey { key_url } if !key_url.is_empty() => {
                format!("API key — create one at {key_url}")
            }
            _ => "API key".to_owned(),
        };
        form = form.child(field(
            &hint,
            boxed(input(&app.key_input).mask_toggle(), theme),
            theme,
        ));
    }
    form = form.child(field(
        "Models",
        boxed(input(&app.models_input), theme),
        theme,
    ));
    if preset.probe {
        form = form.child(div().text_size(px(12.)).text_color(theme.muted_foreground).child(
            "Availability is watched with GET /models every minute (no generation); models the runtime stops serving are hidden.",
        ));
    }
    if let Some(error) = error {
        form = form.child(div().text_color(theme.danger).child(error.to_owned()));
    }
    form.child(
        row()
            .child(
                Button::new("create-provider")
                    .primary()
                    .small()
                    .loading(busy)
                    .label("Add provider")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.create_provider(window, cx)
                    })),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted_foreground)
                    .child("The key is stored on the gateway and never shown again."),
            ),
    )
}

/// A text field on a visible, sunken box.
fn boxed(control: impl IntoElement, theme: &ThemeColor) -> gpui_kit::Div {
    div()
        .px_1()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(control)
}

fn field<E: IntoElement>(name: &str, control: E, theme: &ThemeColor) -> impl IntoElement + use<E> {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.muted_foreground)
                .child(name.to_owned()),
        )
        .child(control)
}

// ----- Models -----------------------------------------------------------------

fn models(state: &InstanceState, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_3().child(
        div()
            .text_color(theme.muted_foreground)
            .child("Turn models off without touching configuration, e.g. while a GPU box runs another loadout."),
    );
    for provider in &state.availability.providers {
        if provider.models.is_empty() {
            continue;
        }
        let probe = state.availability.probe.get(&provider.provider);
        let mut list = div().flex().flex_col();
        for model in &provider.models {
            let deactivation = state
                .availability
                .deactivation(&provider.provider, &model.id);
            let on = deactivation.is_none();
            let (p, m) = (provider.provider.clone(), model.id.clone());
            let entity = cx.entity().downgrade();
            list =
                list.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            Switch::new(SharedString::from(format!("avail-{p}-{m}")))
                                .checked(on)
                                .small()
                                .on_click(move |checked, window, app| {
                                    let (p, m, checked) = (p.clone(), m.clone(), *checked);
                                    let _ = entity.update(app, |this, cx| {
                                        this.set_available(p, m, checked, window, cx)
                                    });
                                }),
                        )
                        .child(div().flex_1().child(model.id.clone()))
                        .when_some(deactivation, |d, why| {
                            d.child(div().text_size(px(11.)).text_color(theme.warning).child(
                                format!(
                                    "{} · by {}",
                                    why.reason.clone().unwrap_or_else(|| "unavailable".into()),
                                    why.set_by
                                ),
                            ))
                        }),
                );
        }
        column = column.child(
            card(theme)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(provider.provider.clone()),
                        )
                        .child(badge(&provider.provider_type, theme.muted_foreground))
                        .when_some(probe, |d, probe| {
                            d.child(if probe.ok {
                                badge("probe ok", theme.success)
                            } else {
                                badge("runtime down", theme.danger)
                            })
                        }),
                )
                .child(list),
        );
    }
    column
}

// ----- Usage ------------------------------------------------------------------

fn usage(state: &InstanceState, theme: &ThemeColor, cx: &mut Context<App>) -> impl IntoElement {
    let mut column = div().flex().flex_col().gap_3().child(
        row()
            .child(div().flex_1().text_color(theme.muted_foreground).child(
                "Plan windows and credits, read from each vendor's own usage endpoint every 5 minutes. No tokens are spent.",
            ))
            .child(
                Button::new("refresh-usage")
                    .outline()
                    .small()
                    .label("Read now")
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh_usage(cx))),
            ),
    );
    match &state.usage {
        Err(message) => column = column.child(notice(theme.warning, message, theme)),
        Ok(accounts) if accounts.is_empty() => {
            column = column.child(empty(
                "No accounts to read",
                "Sign in on the gateway (eavs login anthropic / openai-codex) or add OpenCode Go, OpenRouter or Z.ai.",
                theme,
            ));
        }
        Ok(accounts) => {
            for account in accounts {
                column = column.child(account_card(account, theme));
            }
        }
    }
    column
}

fn account_card(account: &AccountUsage, theme: &ThemeColor) -> impl IntoElement + use<> {
    let status_color = match account.status.as_str() {
        "ok" => theme.success,
        "stale" => theme.warning,
        _ => theme.danger,
    };
    let mut card = card(theme).child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().size(px(8.)).rounded_full().bg(status_color))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(account.provider.clone()),
            )
            .when_some(account.plan.as_deref(), |d, plan| {
                d.child(badge(plan, theme.primary))
            })
            .child(badge(&account.vendor, theme.muted_foreground))
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme.muted_foreground)
                    .child(view::ago(account.observed_at, chrono::Utc::now())),
            ),
    );
    if let Some(message) = &account.message {
        card = card.child(
            div()
                .text_size(px(12.))
                .text_color(theme.muted_foreground)
                .child(message.clone()),
        );
    }
    for meter in &account.meters {
        card = card.child(meter_row(&meter.label, meter, theme));
    }
    card
}

fn meter_row(
    name: &str,
    meter: &eavs_console_core::Meter,
    theme: &ThemeColor,
) -> impl IntoElement + use<> {
    let now = chrono::Utc::now();
    let color = match view::meter_level(meter) {
        view::Level::Full => theme.danger,
        view::Level::High => theme.warning,
        view::Level::Normal => theme.primary,
    };
    let reset = view::reset_text(meter.resets_at, now);
    let pace = view::pace(meter, now)
        .filter(|p| *p > 0.1)
        .map(|_| " · ahead of pace".to_owned())
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .text_size(px(12.))
                .child(div().flex_1().child(name.to_owned()))
                .child(div().child(view::meter_value(meter)))
                .child(
                    div()
                        .ml_2()
                        .text_color(theme.muted_foreground)
                        .child(format!("{reset}{pace}")),
                ),
        )
        .when_some(view::meter_fraction(meter), |d, fraction| {
            d.child(
                Progress::new(SharedString::from(format!("meter-{name}")))
                    .value(fraction * 100.0)
                    .color(color),
            )
        })
}

// ----- Small parts ------------------------------------------------------------

fn card(theme: &ThemeColor) -> gpui_kit::Div {
    div()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .rounded(px(10.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
}

fn row() -> gpui_kit::Div {
    div().flex().items_center().gap_2()
}

fn section_title(text: &str, theme: &ThemeColor) -> impl IntoElement + use<> {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child(text.to_owned())
}

fn label(text: &str, theme: &ThemeColor) -> impl IntoElement + use<> {
    div()
        .px_2()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.muted_foreground)
        .child(text.to_uppercase())
}

fn badge(text: &str, color: Hsla) -> impl IntoElement + use<> {
    div()
        .px_2()
        .rounded(px(999.))
        .text_size(px(11.))
        .bg(color.alpha(0.14))
        .text_color(color)
        .child(text.to_owned())
}

fn notice(color: Hsla, text: &str, theme: &ThemeColor) -> impl IntoElement + use<> {
    div()
        .p_3()
        .rounded(px(8.))
        .border_1()
        .border_color(color.alpha(0.5))
        .bg(color.alpha(0.08))
        .text_color(theme.foreground)
        .child(text.to_owned())
}

fn empty(title: &str, body: &str, theme: &ThemeColor) -> impl IntoElement + use<> {
    card(theme)
        .items_center()
        .py_8()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child(body.to_owned()),
        )
}

fn conn_color(conn: &Conn, theme: &ThemeColor) -> Hsla {
    match conn {
        Conn::Ready => theme.success,
        Conn::Connecting => theme.muted_foreground,
        Conn::Failed(_) => theme.danger,
    }
}

fn host(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).to_owned()
}
