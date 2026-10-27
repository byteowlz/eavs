//! Embedded admin console (a small PWA served at `/admin`).
//!
//! The shell is static UI code with no data and is the only surface exempt
//! from listener authentication; every request the console makes carries the
//! operator token or a maintenance key and goes through the normal
//! authorization path.

/// The single-page console: markup, styles and script inlined.
pub const INDEX: &str = include_str!("admin_pwa/index.html");
/// Installable-web-app manifest.
pub const MANIFEST: &str = include_str!("admin_pwa/manifest.webmanifest");
/// Service worker caching the shell for offline install.
pub const SW: &str = include_str!("admin_pwa/sw.js");
/// Console icon.
pub const ICON: &str = include_str!("admin_pwa/icon.svg");
