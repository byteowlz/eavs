//! Zero-dependency, cross-platform base directory resolution ("option-B").
//!
//! Resolution rule for every base dir:
//! 1. An explicit, absolute `XDG_*` env var wins on ANY OS.
//! 2. Otherwise, on unix (incl. macOS) use `$HOME/<unix_rel>` (e.g. `~/.config`,
//!    NOT `~/Library`).
//! 3. Otherwise, on Windows use the relevant `%APPDATA%` / `%LOCALAPPDATA%`.
//!
//! These helpers replace the previously hand-rolled XDG logic that had no
//! Windows branch. The app name ("eavs") is intentionally NOT joined here so
//! callers keep full control over their subpaths.

use std::path::PathBuf;

/// Core resolver. Kept pure (no env/OS access) so it is fully unit-testable.
fn resolve_base(
    xdg: Option<PathBuf>,
    home: Option<PathBuf>,
    win_dir: Option<PathBuf>,
    is_windows: bool,
    unix_rel: &str,
) -> Option<PathBuf> {
    if let Some(p) = xdg.filter(|p| p.is_absolute()) {
        return Some(p);
    }
    if is_windows {
        win_dir
    } else {
        home.map(|h| h.join(unix_rel))
    }
}

fn nonempty(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn base_dir(xdg_var: &str, unix_rel: &str, win_var: &str) -> anyhow::Result<PathBuf> {
    resolve_base(
        nonempty(xdg_var),
        nonempty("HOME"),
        nonempty(win_var),
        cfg!(windows),
        unix_rel,
    )
    .ok_or_else(|| anyhow::anyhow!("unable to determine base directory ({xdg_var})"))
}

/// Base config dir: `$XDG_CONFIG_HOME` / `~/.config` / `%APPDATA%`.
pub fn config_dir() -> anyhow::Result<PathBuf> {
    base_dir("XDG_CONFIG_HOME", ".config", "APPDATA")
}

/// Base data dir: `$XDG_DATA_HOME` / `~/.local/share` / `%APPDATA%`.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    base_dir("XDG_DATA_HOME", ".local/share", "APPDATA")
}

/// Base state dir: `$XDG_STATE_HOME` / `~/.local/state` / `%LOCALAPPDATA%`.
pub fn state_dir() -> anyhow::Result<PathBuf> {
    base_dir("XDG_STATE_HOME", ".local/state", "LOCALAPPDATA")
}

/// Base cache dir: `$XDG_CACHE_HOME` / `~/.cache` / `%LOCALAPPDATA%`.
#[allow(dead_code)]
pub fn cache_dir() -> anyhow::Result<PathBuf> {
    base_dir("XDG_CACHE_HOME", ".cache", "LOCALAPPDATA")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_xdg_wins_on_any_os() {
        let xdg = Some(PathBuf::from("/custom/state"));
        // unix
        assert_eq!(
            resolve_base(
                xdg.clone(),
                Some(PathBuf::from("/home/u")),
                None,
                false,
                ".local/state"
            ),
            Some(PathBuf::from("/custom/state"))
        );
        // windows
        assert_eq!(
            resolve_base(
                xdg,
                None,
                Some(PathBuf::from(r"C:\Users\u\AppData\Local")),
                true,
                ".local/state"
            ),
            Some(PathBuf::from("/custom/state"))
        );
    }

    #[test]
    fn relative_xdg_is_ignored() {
        // A non-absolute XDG value falls through to the OS default.
        assert_eq!(
            resolve_base(
                Some(PathBuf::from("relative/path")),
                Some(PathBuf::from("/home/u")),
                None,
                false,
                ".config"
            ),
            Some(PathBuf::from("/home/u/.config"))
        );
    }

    #[test]
    fn unix_uses_home_relative() {
        assert_eq!(
            resolve_base(None, Some(PathBuf::from("/home/u")), None, false, ".config"),
            Some(PathBuf::from("/home/u/.config"))
        );
    }

    #[test]
    fn windows_uses_win_dir() {
        let win = Some(PathBuf::from(r"C:\Users\u\AppData\Roaming"));
        assert_eq!(
            resolve_base(
                None,
                Some(PathBuf::from("/home/u")),
                win.clone(),
                true,
                ".config"
            ),
            win
        );
    }

    #[test]
    fn missing_everything_is_none() {
        assert_eq!(resolve_base(None, None, None, false, ".config"), None);
        assert_eq!(resolve_base(None, None, None, true, ".config"), None);
    }
}
