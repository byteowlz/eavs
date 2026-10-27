//! gpui-component adapter: emits a `ThemeSet` JSON document from a scheme +
//! identity. This is the framework vocabulary layer (non-portable): it maps
//! the closed abstract roles (spec/roles.md) onto gpui-component's dotted
//! theme keys so a scheme reskins every compliant tool identically at the
//! role layer, then fills gpui-component's extra tokens (tabs, lists,
//! sidebar, scrollbars, charts, syntax) from the same slots.
//!
//! Emitting JSON instead of gpui types keeps this crate free of gpui, so it
//! can move to `design-system/impls/gpui-rs` unchanged; the app parses the
//! string with `ThemeRegistry::load_themes_from_str`.

use serde_json::{Map, Value, json};

use crate::color::Rgba;
use crate::radius::RadiusScale;
use crate::roles::{Role, Roles};
use crate::scheme::Scheme;

/// Per-tool non-color values plugged into the shared mechanism. Fonts are
/// assembled by font family, so set them to installed family names.
#[derive(Debug, Clone)]
pub struct Identity {
    pub font_family: String,
    pub mono_font_family: String,
    /// Display face for a few accents (section headings, status bar, badges).
    /// Pixel faces render crisply at their native size.
    pub accent_font_family: String,
    pub font_size: f32,
    pub mono_font_size: f32,
    pub radius: RadiusScale,
    pub shadow: bool,
}

impl Default for Identity {
    fn default() -> Self {
        Self {
            font_family: "SystemUIFont".to_string(),
            mono_font_family: "Menlo".to_string(),
            accent_font_family: "SystemUIFont".to_string(),
            font_size: 13.0,
            mono_font_size: 12.0,
            radius: RadiusScale::new(6.0),
            shadow: true,
        }
    }
}

fn hex(c: Rgba) -> Value {
    Value::String(c.to_hex())
}

/// Build the color map for one scheme. Keys are gpui-component's dotted
/// theme keys (see its `default-theme.json`).
pub fn theme_colors(scheme: &Scheme) -> Map<String, Value> {
    let roles = Roles::new(scheme);
    let r = |role: Role| roles.get(role);
    let s = |slot: &str| scheme.slot(slot);
    let dark = scheme.is_dark();

    let background = r(Role::Background);
    let foreground = r(Role::Foreground);
    let surface = r(Role::Surface);
    let sunken = r(Role::SurfaceSunken);
    let muted = r(Role::Muted);
    let muted_fg = r(Role::MutedForeground);
    let border = r(Role::Border);
    let primary = r(Role::Primary);
    let danger = r(Role::Danger);
    let warning = r(Role::Warning);
    let info = r(Role::Info);
    let success = r(Role::Success);
    // Text that sits on a filled accent: the scheme's darkest background in
    // dark mode (web: primary-foreground -> base00), lightest in light mode.
    let on_accent = if dark { s("base00") } else { s("base07") };
    let on_surface_strong = s("base06");
    let hover_step = if dark {
        Rgba::new(1.0, 1.0, 1.0, 1.0)
    } else {
        Rgba::new(0.0, 0.0, 0.0, 1.0)
    };
    let lift = |c: Rgba, t: f32| c.mix(hover_step, t);
    let bright = |normal: &str, bright: &str| if dark { s(bright) } else { s(normal) };

    let mut m = Map::new();
    let mut put = |k: &str, v: Rgba| {
        m.insert(k.to_string(), hex(v));
    };

    // Core surfaces (mirror the abstract roles).
    put("background", background);
    put("foreground", foreground);
    put("border", border);
    put("input.border", r(Role::Input));
    put("ring", r(Role::Ring));
    put("caret", foreground);
    put("selection.background", s("base02").with_alpha(0.8));
    put("muted.background", muted);
    put("muted.foreground", muted_fg);
    put("accent.background", r(Role::Accent));
    put("accent.foreground", on_surface_strong);
    put("secondary.background", r(Role::Secondary));
    put("secondary.hover.background", lift(r(Role::Secondary), 0.06));
    put(
        "secondary.active.background",
        lift(r(Role::Secondary), 0.12),
    );
    put("secondary.foreground", on_surface_strong);
    put("popover.background", surface);
    put("popover.foreground", foreground);
    put("accordion.background", surface);
    put("group_box.background", surface);
    put("group_box.foreground", foreground);
    put("description_list.label.background", surface);
    put("description_list.label.foreground", muted_fg);
    put("overlay", sunken.with_alpha(0.6));
    put("window.border", border);

    // Primary + statuses.
    put("primary.background", primary);
    put("primary.hover.background", lift(primary, 0.08));
    put("primary.active.background", bright("base0B", "base14"));
    put("primary.foreground", on_accent);
    put("success.background", success);
    put("success.hover.background", lift(success, 0.08));
    put("success.active.background", bright("base0B", "base14"));
    put("success.foreground", on_accent);
    put("warning.background", warning);
    put("warning.hover.background", lift(warning, 0.08));
    put("warning.active.background", bright("base0A", "base13"));
    put("warning.foreground", on_accent);
    put("danger.background", danger);
    put("danger.hover.background", lift(danger, 0.08));
    put("danger.active.background", bright("base08", "base12"));
    put("danger.foreground", on_surface_strong);
    put("info.background", info);
    put("info.hover.background", lift(info, 0.08));
    put("info.active.background", bright("base0D", "base16"));
    put("info.foreground", on_accent);

    // Default (ghost/outline) buttons sit on the elevated surface.
    put("button.background", surface);
    put("button.hover.background", lift(surface, 0.06));
    put("button.active.background", lift(surface, 0.12));
    put("button.foreground", foreground);

    // Links.
    put("link", info);
    put("link.hover", bright("base0D", "base16"));
    put("link.active", bright("base0D", "base16"));

    // Sidebar sits on the darkest background (web: --sidebar -> base11).
    put("sidebar.background", sunken);
    put("sidebar.foreground", on_surface_strong);
    put("sidebar.border", border);
    put("sidebar.accent.background", background);
    put("sidebar.accent.foreground", on_surface_strong);
    put("sidebar.primary.background", primary);
    put("sidebar.primary.foreground", sunken);

    // Lists, tables, tabs, title/status bars.
    put("list.background", background);
    put("list.hover.background", surface);
    put("list.active.background", muted);
    put("list.active.border", primary);
    put("list.even.background", background);
    put("list.head.background", surface);
    put("table.background", background);
    put("table.hover.background", surface);
    put("table.active.background", muted);
    put("table.active.border", primary);
    put("table.even.background", background);
    put("table.head.background", surface);
    put("table.head.foreground", muted_fg);
    put("table.foot.background", surface);
    put("table.foot.foreground", muted_fg);
    put("table.row.border", border);
    put("tab_bar.background", surface);
    put("tab_bar.segmented.background", surface);
    put("tab.background", surface.with_alpha(0.0));
    put("tab.foreground", muted_fg);
    put("tab.active.background", background);
    put("tab.active.foreground", primary);
    put("title_bar.background", sunken);
    put("title_bar.border", border);
    put("status_bar.background", surface);
    put("status_bar.border", border);

    // Controls.
    put("skeleton.background", surface);
    put("switch.background", muted);
    put("switch.thumb.background", on_surface_strong);
    put("slider.background", primary);
    put("slider.thumb.background", on_surface_strong);
    put("progress.bar.background", primary);
    put("drag.border", primary);
    put("drop_target.background", primary.with_alpha(0.25));
    put("scrollbar.background", background.with_alpha(0.0));
    put("scrollbar.thumb.background", s("base03").with_alpha(0.6));
    put("scrollbar.thumb.hover.background", s("base03"));

    // Charts: scheme override when present, else a rainbow from the slots.
    let chart_fallback = ["base0D", "base0B", "base0E", "base09", "base0C"];
    for (i, slot) in chart_fallback.iter().enumerate() {
        let key = format!("--chart-{}", i + 1);
        let color = scheme.override_color(&key).unwrap_or_else(|| s(slot));
        put(&format!("chart_{}", i + 1), color);
    }
    put("chart_bullish", success);
    put("chart_bearish", danger);

    // Raw accents.
    put("base.red", s("base08"));
    put("base.red.light", s("base12"));
    put("base.green", s("base0B"));
    put("base.green.light", s("base14"));
    put("base.blue", s("base0D"));
    put("base.blue.light", s("base16"));
    put("base.yellow", s("base0A"));
    put("base.yellow.light", s("base13"));
    put("base.magenta", s("base0E"));
    put("base.magenta.light", s("base17"));
    put("base.cyan", s("base0C"));
    put("base.cyan.light", s("base15"));

    m
}

/// Syntax highlighting for code blocks and editors, from the base24 slots in
/// their conventional roles (comments base03, strings base0B, numbers base09,
/// types base0A, functions base0D, keywords base0E, variables base08,
/// escapes/regex base0C).
pub fn highlight_theme(scheme: &Scheme) -> Value {
    let s = |slot: &str| Value::String(scheme.slot(slot).to_hex());
    let alpha = |slot: &str, a: f32| Value::String(scheme.slot(slot).with_alpha(a).to_hex());
    let color = |slot: &str| json!({ "color": scheme.slot(slot).to_hex() });

    let mut syntax = Map::new();
    for (key, slot) in [
        ("attribute", "base0A"),
        ("boolean", "base09"),
        ("comment", "base03"),
        ("comment.doc", "base03"),
        ("constant", "base09"),
        ("constructor", "base0A"),
        ("embedded", "base05"),
        ("enum", "base0A"),
        ("function", "base0D"),
        ("keyword", "base0E"),
        ("label", "base0C"),
        ("link_text", "base0D"),
        ("link_uri", "base0C"),
        ("number", "base09"),
        ("operator", "base05"),
        ("property", "base08"),
        ("punctuation", "base04"),
        ("punctuation.bracket", "base04"),
        ("punctuation.delimiter", "base04"),
        ("punctuation.list_marker", "base0D"),
        ("punctuation.special", "base0C"),
        ("string", "base0B"),
        ("string.escape", "base0C"),
        ("string.regex", "base0C"),
        ("string.special", "base0C"),
        ("string.special.symbol", "base0C"),
        ("tag", "base08"),
        ("tag.doctype", "base03"),
        ("text.literal", "base0B"),
        ("text.code.span", "base0B"),
        ("type", "base0A"),
        ("variable", "base05"),
        ("variable.special", "base08"),
    ] {
        syntax.insert(key.to_string(), color(slot));
    }
    syntax.insert("emphasis".to_string(), json!({ "font_style": "italic" }));
    syntax.insert("emphasis.strong".to_string(), json!({ "font_weight": 700 }));
    syntax.insert(
        "title".to_string(),
        json!({ "color": scheme.slot("base05").to_hex(), "font_weight": 700 }),
    );

    let mut m = Map::new();
    for (key, value) in [
        ("editor.foreground", s("base05")),
        ("editor.background", s("base00")),
        ("editor.active_line.background", s("base01")),
        ("editor.line_number", s("base03")),
        ("editor.active_line_number", s("base05")),
        ("editor.invisible", s("base02")),
        ("conflict", s("base08")),
        ("created", s("base0B")),
        ("deleted.background", alpha("base08", 0.25)),
        ("created.background", alpha("base0B", 0.25)),
        ("modified", s("base0A")),
        ("hidden", s("base03")),
        ("hint", s("base0E")),
        ("predictive", s("base03")),
    ] {
        m.insert(key.to_string(), value);
    }
    m.insert("syntax".to_string(), Value::Object(syntax));
    Value::Object(m)
}

/// One gpui-component theme entry for a scheme.
pub fn theme_config(scheme: &Scheme, identity: &Identity, is_default: bool) -> Value {
    json!({
        "is_default": is_default,
        "name": scheme.name,
        "mode": if scheme.is_dark() { "dark" } else { "light" },
        "font.family": identity.font_family,
        "font.size": identity.font_size,
        "mono_font.family": identity.mono_font_family,
        "mono_font.size": identity.mono_font_size,
        "radius": identity.radius.lg().round() as u64,
        "radius.lg": identity.radius.xl().round() as u64,
        "shadow": identity.shadow,
        "colors": Value::Object(theme_colors(scheme)),
        "highlight": highlight_theme(scheme),
    })
}

/// A complete gpui-component `ThemeSet` document holding the given schemes.
/// The first scheme of each mode becomes that mode's default.
pub fn theme_set_json(
    name: &str,
    schemes: &[&Scheme],
    identity: &Identity,
) -> Result<String, serde_json::Error> {
    let mut seen_dark = false;
    let mut seen_light = false;
    let themes: Vec<Value> = schemes
        .iter()
        .map(|scheme| {
            let is_default = if scheme.is_dark() {
                !std::mem::replace(&mut seen_dark, true)
            } else {
                !std::mem::replace(&mut seen_light, true)
            };
            theme_config(scheme, identity, is_default)
        })
        .collect();
    let doc = json!({
        "name": name,
        "author": "byteowlz",
        "themes": themes,
    });
    serde_json::to_string_pretty(&doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheme::{example_dark, example_light};

    #[test]
    fn theme_set_has_both_modes_and_role_mapped_colors() {
        let dark = example_dark().unwrap();
        let light = example_light().unwrap();
        let doc = theme_set_json("Example", &[&dark, &light], &Identity::default()).unwrap();
        let v: Value = serde_json::from_str(&doc).unwrap();
        let themes = v["themes"].as_array().unwrap();
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0]["mode"], "dark");
        assert_eq!(themes[0]["is_default"], true);
        assert_eq!(themes[1]["mode"], "light");
        assert_eq!(themes[1]["is_default"], true);
        // Role mapping: background -> base00, primary/success -> base0B.
        assert_eq!(
            themes[0]["colors"]["background"],
            dark.slot("base00").to_hex()
        );
        assert_eq!(
            themes[0]["colors"]["primary.background"],
            dark.slot("base0B").to_hex()
        );
        assert!(themes[0]["highlight"]["editor.background"].is_string());
    }

    #[test]
    fn radius_scales_are_emitted() {
        let dark = example_dark().unwrap();
        let identity = Identity {
            radius: RadiusScale::new(8.0),
            ..Default::default()
        };
        let v: Value =
            serde_json::from_str(&theme_set_json("Example", &[&dark], &identity).unwrap()).unwrap();
        assert_eq!(v["themes"][0]["radius"], 8);
        assert_eq!(v["themes"][0]["radius.lg"], 10);
    }
}
