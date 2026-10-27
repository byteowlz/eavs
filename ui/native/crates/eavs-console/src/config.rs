//! Configuration for a desktop app, loaded from a TOML file at
//! `$XDG_CONFIG_HOME/eavs-console/config.toml` (or
//! `~/.config/eavs-console/config.toml`). A default is written on first
//! run. See `examples/config.schema.json` for the editor/LSP schema.
//!
//! Resolution order is: command-line args > explicit config file > env vars >
//! this file. With no args/env, we use the local config file.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

/// The app name, used as the config directory name and the env-var prefix.
pub const APP_NAME: &str = "eavs-console";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Name of the active design-system scheme (a scheme in the theme set).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Display face used for a few accents (section headings, status bar).
    #[serde(default = "default_accent_font")]
    pub accent_font: String,
    #[serde(default)]
    pub window: WindowConfig,
    /// Seconds between background refreshes of the selected instance.
    #[serde(default = "default_refresh_secs")]
    pub refresh_secs: u64,
    /// Show the gateway on this machine (discovered from eavs' own config).
    #[serde(default = "default_true")]
    pub local_instance: bool,
    /// Other gateways to manage, e.g. the server over a tunnel or the tailnet.
    #[serde(default)]
    pub instances: Vec<eavs_console_core::DeclaredInstance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowConfig {
    #[serde(default = "default_width")]
    pub width: f32,
    #[serde(default = "default_height")]
    pub height: f32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
        }
    }
}

fn default_refresh_secs() -> u64 {
    30
}
const fn default_true() -> bool {
    true
}
fn default_theme() -> String {
    crate::theme::DARK.to_string()
}
fn default_accent_font() -> String {
    "SystemUIFont".to_string()
}
fn default_width() -> f32 {
    1200.0
}
fn default_height() -> f32 {
    800.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            accent_font: default_accent_font(),
            window: WindowConfig {
                width: default_width(),
                height: default_height(),
            },
            refresh_secs: default_refresh_secs(),
            local_instance: true,
            instances: Vec::new(),
        }
    }
}

/// XDG Base Directory Specification, matching the rust-cli template: use
/// `$XDG_CONFIG_HOME` only when it is absolute (unset, empty and relative values
/// are ignored), else `~/.config` (also on macOS), or `%APPDATA%` on Windows.
fn resolve_config_root(
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    is_windows: bool,
) -> PathBuf {
    if let Some(dir) = xdg.filter(|p| p.is_absolute()) {
        return dir;
    }
    let fallback = if is_windows {
        appdata
    } else {
        home.map(|h| h.join(".config"))
    };
    fallback.unwrap_or_else(|| std::env::temp_dir().join(".config"))
}

fn config_root() -> PathBuf {
    resolve_config_root(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        std::env::var_os("APPDATA").map(PathBuf::from),
        cfg!(windows),
    )
}

/// Path to this app's config file.
pub fn config_path() -> PathBuf {
    config_root().join(APP_NAME).join("config.toml")
}

const DEFAULT_TOML: &str = concat!(
    "theme = \"Eavs Control Room\"\n",
    "accent_font = \"SystemUIFont\"\n",
    "refresh_secs = 30\n",
    "# The gateway on this machine is found from ~/.config/eavs/config.toml.\n",
    "local_instance = true\n",
    "\n",
    "[window]\n",
    "width = 1200.0\n",
    "height = 800.0\n",
    "\n",
    "# Manage another gateway (e.g. the server). Credentials are references:\n",
    "# env:NAME or keychain:<account> (keychain service \"eavs\").\n",
    "# [[instances]]\n",
    "# name = \"server\"\n",
    "# url = \"http://127.0.0.1:13044\"   # ssh -L 13044:127.0.0.1:3044 server\n",
    "# token = \"keychain:server-listener-token\"\n",
    "# master_key = \"keychain:server-master-key\"\n",
);

/// Load config, writing a default file on first run.
pub fn load_or_create() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        let parent = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("create config dir {}", parent.display()))?;
        fs::write(&path, DEFAULT_TOML)
            .with_context(|| format!("write default config {}", path.display()))?;
        log::info!("wrote default config to {}", path.display());
    }
    load(&path)
}

/// Load config from a specific file.
pub fn load(path: &Path) -> Result<Config> {
    let builder = config::Config::builder().add_source(config::File::from(path));
    let source = builder
        .build()
        .with_context(|| format!("load {}", path.display()))?;
    let cfg: Config = source.try_deserialize().context("parse config")?;
    Ok(cfg)
}

/// Validate that the default TOML matches the `Config` struct.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_deserializes() {
        let cfg = config::Config::builder()
            .add_source(config::File::from_str(
                DEFAULT_TOML,
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap();
        let cfg: Config = cfg.try_deserialize().unwrap();
        assert_eq!(cfg.theme, "Eavs Control Room");
        assert!(cfg.local_instance);
        assert!(cfg.instances.is_empty());
        assert_eq!(cfg.window.width, 1200.0);
    }

    #[test]
    fn config_root_follows_xdg_spec() {
        let home = Some(PathBuf::from("/home/ada"));
        let resolve = |xdg: Option<&str>| {
            resolve_config_root(xdg.map(PathBuf::from), home.clone(), None, false)
        };
        assert_eq!(
            [
                resolve(Some("/custom")),
                resolve(None),
                resolve(Some("")),
                resolve(Some("rel")),
                resolve(Some("~/c"))
            ],
            [
                PathBuf::from("/custom"),
                PathBuf::from("/home/ada/.config"),
                PathBuf::from("/home/ada/.config"),
                PathBuf::from("/home/ada/.config"),
                PathBuf::from("/home/ada/.config"),
            ]
        );
        assert_eq!(
            resolve_config_root(
                Some("rel".into()),
                home.clone(),
                Some("/appdata".into()),
                true
            ),
            PathBuf::from("/appdata")
        );
    }

    #[test]
    fn config_path_is_under_xdg() {
        assert!(config_path().ends_with("config.toml"));
    }
}
