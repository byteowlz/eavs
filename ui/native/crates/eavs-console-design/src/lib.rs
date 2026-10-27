//! byteowlz design-system mechanism for gpui-kit.
//!
//! Implements the target-agnostic `design-system/spec/` (base24/16 slots,
//! closed role layer, proportional radius, base16 policy) for GPUI. The
//! reusable thing is the **mechanism** — base24 scheme (data) -> abstract
//! `Role` -> a gpui-component `ThemeSet` JSON document — not any one look.
//! Ship your own scheme JSON; this crate only resolves it.
//!
//! This is the candidate for `design-system/impls/gpui-rs`: it holds no gpui
//! types (the adapter emits JSON), so it can move there unchanged.
//!
//! - [`scheme`] parses a scheme file (the only layer a look fills).
//! - [`roles`] resolves the 16 closed `Role`s against a scheme.
//! - [`radius`] is the single-dial proportional scale.
//! - [`theme_set`] emits a gpui-component theme set from a scheme + identity.

pub mod color;
pub mod radius;
pub mod roles;
pub mod scheme;
pub mod theme_set;

pub use color::{Rgba, parse_color};
pub use radius::RadiusScale;
pub use roles::{Role, Roles};
pub use scheme::{
    SLOT_NAMES, Scheme, SchemeFile, SchemeMode, eavs_dark, example_dark, example_light,
};
pub use theme_set::{Identity, theme_set_json};

/// The eavs console theme set: the eavs scheme with the console identity.
pub fn eavs_theme_set_json() -> anyhow::Result<String> {
    let dark = eavs_dark()?;
    let identity = Identity {
        radius: RadiusScale::new(8.0),
        ..Identity::default()
    };
    theme_set_json("Eavs", &[&dark], &identity).map_err(|e| anyhow::anyhow!("theme set: {e}"))
}

/// Build a complete gpui-component `ThemeSet` from this crate's embedded
/// example look (a dark + light scheme pair). This is a **starting point to
/// replace with your own schemes and identity**, not a house look.
pub fn example_theme_set_json() -> anyhow::Result<String> {
    let dark = example_dark()?;
    let light = example_light()?;
    theme_set_json("Example", &[&dark, &light], &Identity::default())
        .map_err(|e| anyhow::anyhow!("theme set: {e}"))
}
