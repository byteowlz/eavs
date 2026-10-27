//! Headless renders of the console with synthetic fixtures (no network, no
//! window shown): `just render <dir>` (a `harness = false` test: AppKit needs the main thread).
//! Metal draws into an image with real text shaping.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eavs_console_core::client::{
    AccountUsage, Availability, Meter, ModelRef, ProbeStatus, ProviderDeactivations,
    ProviderModels, ProviderSource,
};
use eavs_console_core::{Instance, Origin};
use gpui_kit::prelude::*;
use gpui_kit::{AnyWindowHandle, HeadlessAppContext, px, size};

use crate::app::{AddFlow, App, Conn, InstanceState, Section};

fn instance(name: &str, url: &str, origin: Origin) -> Instance {
    Instance {
        name: name.into(),
        url: url.into(),
        token: None,
        master_key: None,
        origin,
    }
}

fn source(name: &str, kind: &str, managed: bool) -> ProviderSource {
    ProviderSource {
        name: name.into(),
        provider_type: kind.into(),
        source: if managed { "admin" } else { "toml" }.into(),
        revision: Some("2026-10-27T10:00:00Z".into()),
        enabled: true,
    }
}

fn models(provider: &str, kind: &str, ids: &[&str], probe: bool) -> ProviderModels {
    ProviderModels {
        provider: provider.into(),
        provider_type: kind.into(),
        probe_enabled: probe,
        models: ids
            .iter()
            .map(|id| ModelRef {
                id: (*id).into(),
                name: String::new(),
            })
            .collect(),
    }
}

fn meter(
    id: &str,
    label: &str,
    unit: &str,
    used: Option<f64>,
    limit: Option<f64>,
    remaining: Option<f64>,
    reset_h: Option<i64>,
    period: Option<u64>,
) -> Meter {
    Meter {
        id: id.into(),
        label: label.into(),
        unit: unit.into(),
        used,
        limit,
        remaining,
        resets_at: reset_h.map(|h| {
            chrono::Utc::now() + chrono::Duration::hours(h) + chrono::Duration::minutes(12)
        }),
        period_secs: period,
    }
}

/// A server-like gateway with local runtimes, subscriptions and API keys.
fn server() -> InstanceState {
    let mut state = InstanceState::new(instance(
        "server",
        "http://127.0.0.1:13044",
        Origin::Declared,
    ));
    state.conn = Conn::Ready;
    state.loaded_at = Some(chrono::Utc::now());
    state.sources = vec![
        source("rtx6000", "openai-compatible", true),
        source("claude", "anthropic", false),
        source("opencode-go", "opencode-go", true),
        source("openrouter", "openrouter", true),
        source("zai", "openai-compatible", true),
    ];
    let mut deactivated = std::collections::BTreeMap::new();
    deactivated.insert(
        "kimi-k3".to_owned(),
        eavs_console_core::client::Deactivation {
            reason: Some("Not served by the runtime (availability probe)".into()),
            until: None,
            set_by: "probe".into(),
        },
    );
    let mut probe = std::collections::BTreeMap::new();
    probe.insert(
        "rtx6000".to_owned(),
        ProbeStatus {
            at: "2026-10-27T10:00:00Z".into(),
            ok: true,
            error: None,
            served: vec!["qwen3.8-flash".into()],
        },
    );
    state.availability = Availability {
        availability: vec![ProviderDeactivations {
            provider: "rtx6000".into(),
            models: deactivated,
        }],
        probe,
        providers: vec![
            models(
                "rtx6000",
                "openai-compatible",
                &["qwen3.8-flash", "kimi-k3"],
                true,
            ),
            models(
                "claude",
                "anthropic",
                &["claude-opus-5-5", "claude-sonnet-5-5"],
                false,
            ),
            models(
                "opencode-go",
                "opencode-go",
                &["glm-5.3", "qwen3.8-plus", "minimax-m3"],
                false,
            ),
            models("zai", "openai-compatible", &["glm-5.3"], false),
        ],
    };
    state.usage = Ok(vec![
        AccountUsage {
            id: "oauth/anthropic/default/default".into(),
            vendor: "anthropic".into(),
            provider: "anthropic (default)".into(),
            account: "default".into(),
            plan: Some("Max".into()),
            status: "ok".into(),
            message: None,
            observed_at: Some(chrono::Utc::now() - chrono::Duration::minutes(2)),
            meters: vec![
                meter(
                    "session",
                    "Session",
                    "percent",
                    Some(23.0),
                    Some(100.0),
                    None,
                    Some(3),
                    Some(5 * 3600),
                ),
                meter(
                    "weekly",
                    "Weekly",
                    "percent",
                    Some(84.0),
                    Some(100.0),
                    None,
                    Some(52),
                    Some(7 * 86_400),
                ),
                meter(
                    "extra_usage",
                    "Extra usage",
                    "usd",
                    Some(12.34),
                    Some(50.0),
                    Some(37.66),
                    None,
                    None,
                ),
            ],
        },
        AccountUsage {
            id: "provider/opencode-go".into(),
            vendor: "opencode-go".into(),
            provider: "opencode-go".into(),
            account: "default".into(),
            plan: Some("Go".into()),
            status: "stale".into(),
            message: Some("rate limited by the vendor; retrying in about 15 min".into()),
            observed_at: Some(chrono::Utc::now() - chrono::Duration::minutes(17)),
            meters: vec![
                meter(
                    "session",
                    "Session",
                    "percent",
                    Some(3.0),
                    Some(100.0),
                    None,
                    Some(4),
                    Some(5 * 3600),
                ),
                meter(
                    "weekly",
                    "Weekly",
                    "percent",
                    Some(40.0),
                    Some(100.0),
                    None,
                    Some(110),
                    Some(7 * 86_400),
                ),
                meter(
                    "monthly",
                    "Monthly",
                    "percent",
                    Some(100.0),
                    Some(100.0),
                    None,
                    Some(300),
                    Some(30 * 86_400),
                ),
            ],
        },
        AccountUsage {
            id: "provider/openrouter".into(),
            vendor: "openrouter".into(),
            provider: "openrouter".into(),
            account: "default".into(),
            plan: None,
            status: "ok".into(),
            message: None,
            observed_at: Some(chrono::Utc::now()),
            meters: vec![meter(
                "balance",
                "Credits",
                "usd",
                Some(12.5),
                Some(50.0),
                Some(37.5),
                None,
                None,
            )],
        },
    ]);
    state
}

fn laptop() -> InstanceState {
    let mut state = InstanceState::new(instance(
        "This machine",
        "http://127.0.0.1:3044",
        Origin::Local,
    ));
    state.conn = Conn::Ready;
    state
}

fn headless() -> HeadlessAppContext {
    let text_system = gpui_kit::platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_kit::assets::AllAssets),
        gpui_kit::platform::current_headless_renderer,
    );
    cx.allow_parking();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::init(cx, &crate::config::Config::default());
    });
    cx
}

fn capture(cx: &mut HeadlessAppContext, window: AnyWindowHandle, path: &Path) {
    for _ in 0..4 {
        cx.run_until_parked();
        let _ = cx.update_window(window, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }
    let image = cx
        .capture_screenshot(window)
        .expect("Metal headless render");
    image.save(path).expect("write png");
    eprintln!("wrote {}", path.display());
}

fn shot(
    dir: &Path,
    file: &str,
    setup: impl Fn(&mut App, &mut gpui_kit::Window, &mut gpui_kit::Context<App>) + 'static,
) {
    let mut cx = headless();
    let window: AnyWindowHandle = cx
        .open_window(size(px(1200.), px(800.)), |window, cx| {
            let app = cx.new(|cx| {
                let mut app = App::build(
                    vec![laptop(), server()],
                    Vec::new(),
                    Duration::from_secs(30),
                    window,
                    cx,
                );
                app.selected = 1;
                setup(&mut app, window, cx);
                app
            });
            cx.new(|cx| gpui_kit::component::Root::new(app, window, cx))
        })
        .expect("headless window")
        .into();
    capture(&mut cx, window, &dir.join(file));
}

pub fn run() {
    let Some(dir) = std::env::var_os("EAVS_CONSOLE_RENDER") else {
        eprintln!("render: skipped (set EAVS_CONSOLE_RENDER=<dir>)");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("render dir");
    shot(&dir, "overview.png", |app, _, _| {
        app.section = Section::Overview
    });
    shot(&dir, "providers.png", |app, _, _| {
        app.section = Section::Providers;
        app.expanded = Some("rtx6000".into());
    });
    shot(&dir, "models.png", |app, _, _| {
        app.section = Section::Models
    });
    shot(&dir, "usage.png", |app, _, _| app.section = Section::Usage);
    shot(&dir, "add-provider.png", |app, window, cx| {
        app.add = Some(AddFlow {
            preset: None,
            error: None,
            busy: false,
        });
        app.choose_preset(&eavs_console_core::PRESETS[3], window, cx);
    });
}
