use super::{model::*, render};
use crate::fleet::{Change, FleetConfig, Instance, ProviderSource};
use byteowlz_tui_kit::prelude::*;
use ratatui::{backend::TestBackend, Terminal};

fn app() -> App {
    let mut app = App::new(FleetConfig {
        instances: vec![
            Instance {
                name: "dev-east".into(),
                url: "https://east.example.test".into(),
                admin_key_env: "EAST_ADMIN".into(),
            },
            Instance {
                name: "dev-west".into(),
                url: "https://west.example.test".into(),
                admin_key_env: "WEST_ADMIN".into(),
            },
        ],
    });
    for machine in &mut app.machines {
        machine.loaded = true;
        machine.status = "Ready".into();
        machine.providers = vec![ProviderSource {
            name: "openai".into(),
            provider_type: "openai".into(),
            source: "admin".into(),
            revision: Some("pinned-revision".into()),
            enabled: true,
        }];
    }
    app
}

#[test]
fn program_status_uses_only_current_batch_targets() {
    let mut app = app();
    app.machines[0].result = Some("Failed: prior operation".into());
    app.machines[1].result = Some("Applied".into());
    let mut output = Vec::new();
    {
        let mut reporter = crate::program_status::Reporter::new(&mut output, true);
        super::report_settled(&app, Some(&[1]), &mut reporter);
    }
    assert!(output.starts_with(b"\x1b]7501;state=done:"));
    output.clear();
    {
        let mut reporter = crate::program_status::Reporter::new(&mut output, true);
        super::report_settled(&app, Some(&[0]), &mut reporter);
    }
    assert!(output.starts_with(b"\x1b]7501;state=error:"));
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify no pre-confirmation mutation and pinned revisions; missing fixtures propagate"
)]
fn rotate_requires_preview_confirmation_and_retains_revision(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = app();
    let actions = actions();
    let mut router = KeyRouter::new(&actions);
    assert!(matches!(
        app.input(Key::char('c'), &mut router, &actions),
        Effect::None
    ));
    let Some(Overlay::Form(form)) = app.overlay.as_mut() else {
        return Err("expected rotate form".into());
    };
    form.fields[1].1 = "LOCAL_PROVIDER_KEY".into();
    assert!(matches!(
        app.input(Key::enter(), &mut router, &actions),
        Effect::None
    ));
    assert!(matches!(app.overlay, Some(Overlay::Preview(_))));
    app.machines[0].providers[0].revision = Some("new-revision".into());
    let Effect::Apply(targets) = app.input(Key::enter(), &mut router, &actions) else {
        return Err("confirmation must produce a plan".into());
    };
    assert_eq!(targets.len(), 1);
    assert!(
        matches!(&targets[0].change, Some(Change::Rotate { revision, key_env, .. }) if revision == "pinned-revision" && key_env == "LOCAL_PROVIDER_KEY")
    );
    Ok(())
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify canceled and ineligible plans; missing fixtures propagate"
)]
fn cancellation_toml_and_unavailable_targets_never_apply() -> Result<(), Box<dyn std::error::Error>>
{
    let mut app = app();
    let actions = actions();
    let mut router = KeyRouter::new(&actions);
    app.instances.select_all(2);
    app.machines[0].providers[0].source = "toml".into();
    app.machines[1].loaded = false;
    assert!(matches!(app.action("rollback"), Effect::None));
    let Some(Overlay::Preview(targets)) = &app.overlay else {
        return Err("expected preview".into());
    };
    assert_eq!(targets.len(), 2);
    assert!(targets.iter().all(|target| target.change.is_none()));
    assert!(matches!(
        app.input(Key::esc(), &mut router, &actions),
        Effect::None
    ));
    assert!(app.overlay.is_none());
    assert!(matches!(
        app.input(Key::enter(), &mut router, &actions),
        Effect::None
    ));
    Ok(())
}

#[test]
fn navigation_selection_and_palette_share_actions() {
    let mut app = app();
    let actions = actions();
    let mut router = KeyRouter::new(&actions);
    let _ = app.input(Key::space(), &mut router, &actions);
    let _ = app.input(Key::down(), &mut router, &actions);
    assert_eq!(app.instances.index(), 1);
    assert_eq!(app.targets(), vec![0]);
    let _ = app.input(Key::tab(), &mut router, &actions);
    assert!(app.provider_focus);
    let _ = app.input(Key::char(':'), &mut router, &actions);
    assert!(app.palette.is_some());
    let _ = app.input(Key::esc(), &mut router, &actions);
    assert!(app.palette.is_none());
    app.busy = true;
    assert!(matches!(app.action("quit"), Effect::None));
    assert!(matches!(app.action("rotate"), Effect::None));
}

#[test]
#[expect(
    clippy::panic_in_result_fn,
    reason = "Assertions verify real widget structure and snapshot contents; rendering errors propagate"
)]
fn fleet_frame_has_panels_bar_fill_and_secret_free_preview(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = app();
    app.instances.select_all(2);
    let mut terminal = Terminal::new(TestBackend::new(110, 26))?;
    terminal.draw(|frame| render::draw(frame, &mut app))?;
    let buffer = terminal.backend().buffer();
    let mut text = String::new();
    for y in 0..26 {
        for x in 0..110 {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    assert!(text.contains("EAVS / Fleet"));
    assert!(text.contains("Instances"));
    assert!(text.contains("Providers · dev-east"));
    assert!(text.contains("╭"));
    assert!(text.contains("pinned-revision"));
    assert_eq!(buffer[(0, 0)].bg, ratatui::style::Color::Black);
    assert!(!text.contains("EAST_ADMIN"));
    if let Ok(path) = std::env::var("EAVS_TUI_SNAPSHOT") {
        let cells: Vec<_> = (0..26).flat_map(|y| (0..110).map(move |x| {
            let cell = &buffer[(x,y)];
            serde_json::json!({"x":x,"y":y,"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})
        })).collect();
        std::fs::write(path, serde_json::to_vec(&cells)?)?;
    }
    Ok(())
}
