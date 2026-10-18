use super::model::{App, Overlay};
use byteowlz_tui_kit::{
    prelude::*,
    widgets::{bar, panel},
};
use ratatui::{
    layout::{Constraint, Layout},
    text::{Line, Span},
    widgets::{Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let theme = Theme::ansi_default();
    if frame.area().width < 60 || frame.area().height < 16 {
        frame.render_widget(
            Paragraph::new("Resize terminal to at least 60 × 16; q quits")
                .style(theme.fg(Token::Warning)),
            frame.area(),
        );
        return;
    }
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .split(frame.area());
    let header = Line::from(vec![
        Span::styled(" EAVS / Fleet ", theme.on_bar_bold(Token::Accent)),
        Span::styled(
            format!(
                "  {} selected · {}",
                app.instances.selected().len(),
                if app.busy { "working" } else { "idle" }
            ),
            theme.on_bar_bold(Token::Primary),
        ),
    ]);
    frame.render_widget(bar(header, theme), areas[0]);
    let panes = Layout::horizontal([
        Constraint::Percentage(30),
        Constraint::Length(1),
        Constraint::Percentage(70),
    ])
    .split(areas[1]);
    draw_instances(frame, app, panes[0], theme);
    draw_providers(frame, app, panes[2], theme);
    frame.render_widget(
        Paragraph::new(clean(&app.status)).style(theme.fg(Token::Muted)),
        ratatui::layout::Rect::new(areas[2].x, areas[2].y, areas[2].width, 1),
    );
    draw_status_bar(
        frame,
        ratatui::layout::Rect::new(areas[2].x, areas[2].y + 1, areas[2].width, 1),
        theme,
        "Space select · Tab focus",
        &[
            ("a", "add"),
            ("c", "rotate"),
            ("u", "rollback"),
            (":", "commands"),
            ("q", "quit"),
        ],
    );
    draw_overlay(frame, app, theme);
    if let Some(palette) = app.palette.as_mut() {
        palette.draw(frame, theme);
    }
}

fn draw_instances(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect, theme: Theme) {
    let items: Vec<_> = app
        .fleet
        .instances
        .iter()
        .enumerate()
        .map(|(index, instance)| {
            let selected = if app.instances.is_selected(index) {
                "[x]"
            } else {
                "[ ]"
            };
            let status = app.machines.get(index).map_or("unavailable", |machine| {
                machine.result.as_deref().unwrap_or(&machine.status)
            });
            ListItem::new(vec![
                Line::from(format!("{selected} {}", clean(&instance.name))),
                Line::from(Span::styled(
                    format!("  {}", clean(status)),
                    theme.fg(Token::Muted),
                )),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel("Instances", theme, !app.provider_focus))
        .highlight_style(if app.provider_focus {
            theme.fg_bold(Token::Primary)
        } else {
            theme.focus()
        })
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, &mut app.instances.state());
}

fn draw_providers(frame: &mut Frame<'_>, app: &App, area: ratatui::layout::Rect, theme: Theme) {
    let title = app.fleet.instances.get(app.instances.index()).map_or_else(
        || "Providers".into(),
        |instance| format!("Providers · {}", clean(&instance.name)),
    );
    let block = panel(&title, theme, app.provider_focus);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(machine) = app.machines.get(app.instances.index()) else {
        return;
    };
    if machine.providers.is_empty() {
        let prompt = if machine.loaded {
            "No providers — a add, : commands"
        } else {
            "Inventory unavailable — r refresh"
        };
        draw_empty_state(frame, inner, theme, prompt);
        return;
    }
    let split = Layout::vertical([Constraint::Min(2), Constraint::Length(5)]).split(inner);
    let items: Vec<_> = machine
        .providers
        .iter()
        .map(|provider| {
            let source = if provider.source == "toml" {
                "TOML / read-only"
            } else {
                "admin"
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", clean(&provider.name)),
                    theme.fg_bold(Token::Primary),
                ),
                Span::styled(
                    format!(
                        "{} · {source} · {}",
                        clean(&provider.provider_type),
                        if provider.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    ),
                    theme.fg(Token::Muted),
                ),
            ]))
        })
        .collect();
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(if app.provider_focus {
                theme.focus()
            } else {
                theme.fg_bold(Token::Primary)
            })
            .highlight_symbol("› "),
        split[0],
        &mut app.providers.state(),
    );
    if let Some(provider) = app.provider() {
        let detail = format!(
            "Source: {}\nRevision: {}\nActions target selected instances; no keys are displayed.",
            clean(&provider.source),
            provider
                .revision
                .as_deref()
                .map_or_else(|| "TOML-managed".to_string(), clean)
        );
        frame.render_widget(
            Paragraph::new(detail)
                .style(theme.fg(Token::Muted))
                .wrap(Wrap { trim: true }),
            split[1],
        );
    }
}

fn draw_overlay(frame: &mut Frame<'_>, app: &App, theme: Theme) {
    let Some(overlay) = &app.overlay else {
        return;
    };
    let area = centered_rect(85, 75, frame.area());
    frame.render_widget(Clear, area);
    match overlay {
        Overlay::Form(form) => {
            let mut lines = vec![
                Line::from("Local environment reference only — never paste a key."),
                Line::from(""),
            ];
            for (index, (label, value)) in form.fields.iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    format!("{label}: {}", clean(value)),
                    if index == form.cursor {
                        theme.focus()
                    } else {
                        theme.fg(Token::Primary)
                    },
                )));
                lines.push(Line::from(""));
            }
            lines.push(Line::from("Tab/↑/↓ field · Enter preview · Esc cancel"));
            frame.render_widget(
                Paragraph::new(lines)
                    .style(theme.bg(Token::Bar))
                    .block(panel("Provider change", theme, true))
                    .wrap(Wrap { trim: true }),
                area,
            );
        }
        Overlay::Preview(targets) => {
            let block = panel("Confirm apply", theme, true).style(theme.bg(Token::Bar));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let sections = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(inner);
            let eligible = targets
                .iter()
                .filter(|target| target.change.is_some())
                .count();
            frame.render_widget(Paragraph::new(format!("{} targets · {eligible} eligible · pinned revisions\n↑/↓ or PgUp/PgDn scroll review", targets.len())).style(theme.fg_bold(Token::Primary)), sections[0]);
            let mut lines = Vec::new();
            for target in targets {
                let name = app
                    .fleet
                    .instances
                    .get(target.index)
                    .map_or("unknown", |instance| instance.name.as_str());
                lines.push(Line::from(Span::styled(
                    format!("{} · {}", clean(name), clean(&target.description)),
                    if target.change.is_some() {
                        theme.fg(Token::Primary)
                    } else {
                        theme.fg(Token::Warning)
                    },
                )));
                lines.push(Line::from(""));
            }
            frame.render_widget(
                Paragraph::new(lines)
                    .scroll((app.preview_scroll, 0))
                    .wrap(Wrap { trim: true }),
                sections[1],
            );
            frame.render_widget(Paragraph::new("Enter applies eligible targets independently · Esc cancels\nNo cross-instance transaction; no automatic retry.").style(theme.fg(Token::Muted)).wrap(Wrap { trim: true }), sections[2]);
        }
    }
}

pub fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}
