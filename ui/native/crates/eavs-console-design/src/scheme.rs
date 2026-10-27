//! A base24 (or base16) scheme: the only layer a look fills. Format follows
//! `design-system/spec/schema.json`; ship your own scheme JSON and parse it
//! here. base16 schemes are widened to base24 by the derivation policy in
//! `design-system/spec/base16-policy.md`.

use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::color::{Rgba, parse_color};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemeMode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchemeFile {
    pub id: String,
    pub name: String,
    pub mode: SchemeMode,
    #[serde(default)]
    pub system: Option<String>,
    pub slots: BTreeMap<String, String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, String>,
}

/// A scheme with every slot resolved to sRGB. base16 schemes are widened to
/// base24 by the derivation policy in `spec/base16-policy.md` (bright accents
/// copy their normal accent; `base10`/`base11` step darker from `base00`).
#[derive(Debug, Clone)]
pub struct Scheme {
    pub id: String,
    pub name: String,
    pub mode: SchemeMode,
    slots: BTreeMap<String, Rgba>,
    pub overrides: BTreeMap<String, Rgba>,
}

pub const SLOT_NAMES: [&str; 24] = [
    "base00", "base01", "base02", "base03", "base04", "base05", "base06", "base07", "base08",
    "base09", "base0A", "base0B", "base0C", "base0D", "base0E", "base0F", "base10", "base11",
    "base12", "base13", "base14", "base15", "base16", "base17",
];

impl Scheme {
    pub fn parse(json: &str) -> Result<Self> {
        let file: SchemeFile = serde_json::from_str(json).context("scheme JSON")?;
        Self::from_file(file)
    }

    pub fn from_file(file: SchemeFile) -> Result<Self> {
        let mut slots = BTreeMap::new();
        for (name, value) in &file.slots {
            let color = parse_color(value).with_context(|| format!("slot {name}"))?;
            slots.insert(name.clone(), color);
        }
        for name in &SLOT_NAMES[..16] {
            if !slots.contains_key(*name) {
                return Err(anyhow!("scheme {} is missing {name}", file.id));
            }
        }
        derive_base24(&mut slots, file.mode);
        let mut overrides = BTreeMap::new();
        for (name, value) in &file.overrides {
            let color = parse_color(value).with_context(|| format!("override {name}"))?;
            overrides.insert(name.clone(), color);
        }
        Ok(Self {
            id: file.id,
            name: file.name,
            mode: file.mode,
            slots,
            overrides,
        })
    }

    /// Resolved slot color. The 16 base slots are validated at parse and
    /// `base10`-`base17` are derived, so every `baseXX` is present.
    pub fn slot(&self, name: &str) -> Rgba {
        self.slots[name]
    }

    pub fn is_dark(&self) -> bool {
        self.mode == SchemeMode::Dark
    }

    pub fn override_color(&self, name: &str) -> Option<Rgba> {
        self.overrides.get(name).copied()
    }
}

fn derive_base24(slots: &mut BTreeMap<String, Rgba>, mode: SchemeMode) {
    let base00 = slots["base00"];
    let toward = if mode == SchemeMode::Dark {
        Rgba::new(0.0, 0.0, 0.0, 1.0)
    } else {
        Rgba::new(1.0, 1.0, 1.0, 1.0)
    };
    let derived: [(&str, Rgba); 8] = [
        ("base10", base00.mix(toward, 0.25)),
        ("base11", base00.mix(toward, 0.4)),
        ("base12", slots["base08"]),
        ("base13", slots["base0A"]),
        ("base14", slots["base0B"]),
        ("base15", slots["base0C"]),
        ("base16", slots["base0D"]),
        ("base17", slots["base0E"]),
    ];
    for (name, color) in derived {
        slots.entry(name.to_string()).or_insert(color);
    }
}

const EAVS_DARK_JSON: &str = include_str!("../schemes/eavs-dark.json");
const EXAMPLE_DARK_JSON: &str = include_str!("../schemes/example-dark.json");
const EXAMPLE_LIGHT_JSON: &str = include_str!("../schemes/example-light.json");

/// The eavs graphite/lime scheme (ADR-0008), the app's look.
pub fn eavs_dark() -> Result<Scheme> {
    Scheme::parse(EAVS_DARK_JSON)
}

/// The embedded example dark scheme (validated by `example_schemes_parse`).
pub fn example_dark() -> Result<Scheme> {
    Scheme::parse(EXAMPLE_DARK_JSON)
}

/// The embedded example light scheme (validated by `example_schemes_parse`).
pub fn example_light() -> Result<Scheme> {
    Scheme::parse(EXAMPLE_LIGHT_JSON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eavs_scheme_parses() {
        let dark = eavs_dark().unwrap();
        assert!(dark.is_dark());
        assert_eq!(dark.slot("base0B").to_hex(), "#d4f477");
    }

    #[test]
    fn example_schemes_parse() {
        let dark = example_dark().unwrap();
        assert!(dark.is_dark());
        assert!(dark.slot("base00").to_hex().starts_with('#'));
        let light = example_light().unwrap();
        assert!(!light.is_dark());
        assert!(light.slot("base00").luminance() > light.slot("base05").luminance());
    }

    #[test]
    fn base16_is_widened() {
        let mut file: SchemeFile = serde_json::from_str(EXAMPLE_DARK_JSON).unwrap();
        for name in &SLOT_NAMES[16..] {
            file.slots.remove(*name);
        }
        let scheme = Scheme::from_file(file).unwrap();
        assert_eq!(scheme.slot("base14"), scheme.slot("base0B"));
        assert!(scheme.slot("base11").luminance() < scheme.slot("base00").luminance());
    }

    #[test]
    fn missing_base_slot_is_rejected() {
        let mut file: SchemeFile = serde_json::from_str(EXAMPLE_DARK_JSON).unwrap();
        file.slots.remove("base0B");
        assert!(Scheme::from_file(file).is_err());
    }

    #[test]
    fn all_roles_resolve_to_a_present_slot() {
        let dark = example_dark().unwrap();
        for role in crate::roles::Role::ALL {
            let _ = dark.slot(role.slot());
        }
    }
}
