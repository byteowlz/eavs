//! Fleet TUI: a view over the same operations as `eavs fleet`.
mod model;
mod render;
#[cfg(test)]
mod tests;

use crate::fleet::{self, ProviderSource};
use crate::program_status::{Reporter, State};
use anyhow::{bail, Result};
use byteowlz_tui_kit::prelude::*;
use futures::StreamExt;
use model::{App, Effect, Overlay, Target};
use std::{io::IsTerminal, path::Path, time::Duration};
use tokio::sync::mpsc;

enum Update {
    Inventory(usize, Result<Vec<ProviderSource>, String>),
    Applied(usize, String),
    Done,
}

pub async fn run(path: &Path) -> Result<()> {
    let enabled = crate::program_status::enabled_for_terminal(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
    );
    let mut reporter = Reporter::new(std::io::stdout(), enabled);
    let result = run_inner(path, &mut reporter).await;
    if result.is_err() {
        reporter.set(
            State::Error,
            "Fleet TUI stopped; inspect terminal diagnostics",
        );
    } else {
        reporter.set(State::Idle, "Fleet TUI closed");
    }
    result
}

async fn run_inner(path: &Path, reporter: &mut Reporter<std::io::Stdout>) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("TUI requires an interactive terminal; use `eavs fleet --help` for scripting");
    }
    let config = fleet::load_config(path)?;
    let mut app = App::new(config);
    let registry = model::actions();
    let mut router = KeyRouter::new(&registry);
    let (sender, mut receiver) = mpsc::channel(64);
    let mut terminal = TerminalGuard::enter()?;
    let mut applying: Option<Vec<usize>> = None;
    reporter.set(State::Working, "Reading fleet inventories");
    start_refresh(&mut app, sender.clone());
    loop {
        while let Ok(update) = receiver.try_recv() {
            let settled = matches!(&update, Update::Done);
            update_app(&mut app, update);
            if settled {
                report_settled(&app, applying.as_deref(), reporter);
            }
        }
        terminal.draw(|frame| render::draw(frame, &mut app))?;
        let event = tokio::task::spawn_blocking(|| poll_event(Duration::from_millis(80))).await??;
        if let Some(AppEvent::Key(key)) = event {
            match app.input(key, &mut router, &registry) {
                Effect::Quit => break,
                Effect::Refresh => {
                    applying = None;
                    reporter.set(State::Working, "Reading fleet inventories");
                    start_refresh(&mut app, sender.clone());
                }
                Effect::Apply(targets) => {
                    applying = Some(targets.iter().map(|target| target.index).collect());
                    reporter.set(State::Working, "Applying confirmed fleet plan");
                    start_apply(&mut app, targets, sender.clone());
                }
                Effect::None => {}
            }
            if !app.busy {
                match &app.overlay {
                    Some(Overlay::Form(_)) => reporter.set(
                        State::Question,
                        "Complete provider form; no changes applied",
                    ),
                    Some(Overlay::Preview(_)) => reporter.set(
                        State::Permission,
                        "Review fleet plan; Enter applies eligible targets, Escape cancels",
                    ),
                    _ => reporter.set(State::Idle, "Browsing fleet"),
                }
            }
        }
    }
    Ok(())
}

fn report_settled<W: std::io::Write>(
    app: &App,
    applying: Option<&[usize]>,
    reporter: &mut Reporter<W>,
) {
    let Some(indices) = applying else {
        reporter.set(State::Idle, "Fleet inventory read settled");
        return;
    };
    let failed = indices
        .iter()
        .filter_map(|index| app.machines.get(*index))
        .any(|machine| {
            machine
                .result
                .as_ref()
                .is_some_and(|result| result.starts_with("Failed:"))
        });
    reporter.set(
        if failed { State::Error } else { State::Done },
        "Fleet operation settled; inspect per-instance results",
    );
}

fn start_refresh(app: &mut App, sender: mpsc::Sender<Update>) {
    app.busy = true;
    app.status = "Refreshing inventories; no mutations".into();
    for machine in &mut app.machines {
        machine.result = None;
    }
    let instances = app.fleet.instances.clone();
    tokio::spawn(async move {
        futures::stream::iter(instances.into_iter().enumerate())
            .for_each_concurrent(4, |(index, instance)| {
                let sender = sender.clone();
                async move {
                    let result = fleet::inventory(&instance)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = sender.send(Update::Inventory(index, result)).await;
                }
            })
            .await;
        let _ = sender.send(Update::Done).await;
    });
}

fn start_apply(app: &mut App, targets: Vec<Target>, sender: mpsc::Sender<Update>) {
    app.busy = true;
    app.status = "Applying confirmed plan; each instance reports independently".into();
    let instances = app.fleet.instances.clone();
    tokio::spawn(async move {
        for target in targets {
            let Some(instance) = instances.get(target.index) else {
                continue;
            };
            let Some(change) = target.change else {
                let _ = sender
                    .send(Update::Applied(
                        target.index,
                        format!("Skipped: {}", target.description),
                    ))
                    .await;
                continue;
            };
            let status = match fleet::apply(instance, &change).await {
                Ok(()) => "Applied".to_string(),
                Err(error) => format!("Failed: {error}"),
            };
            let applied = status.starts_with("Applied");
            let _ = sender.send(Update::Applied(target.index, status)).await;
            if applied {
                let inventory = fleet::inventory(instance)
                    .await
                    .map_err(|error| error.to_string());
                let _ = sender
                    .send(Update::Inventory(target.index, inventory))
                    .await;
            }
        }
        let _ = sender.send(Update::Done).await;
    });
}

fn update_app(app: &mut App, update: Update) {
    match update {
        Update::Inventory(index, result) => {
            if let Some(machine) = app.machines.get_mut(index) {
                match result {
                    Ok(providers) => {
                        machine.providers = providers;
                        machine.loaded = true;
                        machine.status = "Ready".into();
                    }
                    Err(error) => {
                        machine.loaded = false;
                        machine.providers.clear();
                        machine.status = format!("Unavailable: {error}");
                    }
                }
            }
        }
        Update::Applied(index, status) => {
            if let Some(machine) = app.machines.get_mut(index) {
                machine.result = Some(status);
            }
        }
        Update::Done => {
            app.busy = false;
            app.status = "Finished — per-instance results shown; r refresh, : commands".into();
        }
    }
}
