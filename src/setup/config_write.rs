//! Cooperative, serialized, validated atomic setup writes. No secret-bearing parser errors.
use crate::cli::CliError;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

fn error(message: &str) -> CliError {
    CliError::Other(message.into())
}

fn transaction<T>(
    path: &Path,
    change: impl FnOnce(&str, &toml::Value) -> Result<(Option<String>, T), CliError>,
) -> Result<T, CliError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|_| error("Cannot create configuration directory"))?;
    let destination = match path.canonicalize() {
        Ok(path) => path,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => {
            if fs::symlink_metadata(path).is_ok() {
                return Err(error("Cannot update a dangling config symlink"));
            }
            parent
                .canonicalize()
                .map_err(|_| error("Cannot resolve configuration directory"))?
                .join(
                    path.file_name()
                        .ok_or_else(|| error("Config path must name a file"))?,
                )
        }
        Err(_) => return Err(error("Cannot resolve configuration path")),
    };
    let mut lock_path = destination.as_os_str().to_os_string();
    lock_path.push(".setup.lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let lock = options
        .open(lock_path)
        .map_err(|_| error("Cannot open setup config lock"))?;
    lock.lock()
        .map_err(|_| error("Cannot lock setup configuration"))?;
    let original = match fs::read_to_string(&destination) {
        Ok(text) => text,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err(error("Cannot read existing configuration; file unchanged")),
    };
    let document: toml::Value = original.parse().map_err(|_| error("Existing config is invalid TOML; repair duplicate/malformed sections first. File unchanged"))?;
    let (candidate, result) = change(&original, &document)?;
    let Some(candidate) = candidate else {
        return Ok(result);
    };
    candidate
        .parse::<toml::Value>()
        .map_err(|_| error("Proposed configuration is invalid TOML; file unchanged"))?;
    let parent = destination
        .parent()
        .ok_or_else(|| error("Config path has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| error("Cannot create atomic config temporary file"))?;
    if let Ok(metadata) = fs::metadata(&destination) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let replacement = temporary
                .as_file()
                .metadata()
                .map_err(|_| error("Cannot inspect temporary file"))?;
            if metadata.uid() != replacement.uid() || metadata.gid() != replacement.gid() {
                return Err(error(
                    "Atomic replacement cannot preserve config owner/group; file unchanged",
                ));
            }
        }
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|_| error("Cannot preserve config permissions"))?;
    }
    temporary
        .write_all(candidate.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| error("Cannot persist proposed configuration; file unchanged"))?;
    // Also catch a non-cooperating editor's changes observed before replacement.
    let current = match fs::read_to_string(&destination) {
        Ok(text) => text,
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound && original.is_empty() => {
            String::new()
        }
        Err(_) => return Err(error("Config changed or became unreadable; file unchanged")),
    };
    if current != original {
        return Err(error(
            "Config changed during setup; refresh and retry. File unchanged",
        ));
    }
    temporary
        .persist(&destination)
        .map_err(|_| error("Cannot atomically replace config"))?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| {
            error("Config saved, but directory synchronization failed; inspect before retrying")
        })?;
    Ok(result)
}

fn append_text(original: &str, providers: toml::Table) -> Result<String, CliError> {
    let mut root = toml::Table::new();
    root.insert("providers".into(), toml::Value::Table(providers));
    let snippet =
        toml::to_string(&root).map_err(|_| error("Cannot serialize provider declaration"))?;
    Ok(format!("{original}\n{snippet}"))
}

pub(super) fn contains_provider(path: &Path, name: &str) -> Result<bool, CliError> {
    transaction(path, |_, document| {
        Ok((
            None,
            document
                .get("providers")
                .and_then(toml::Value::as_table)
                .is_some_and(|providers| providers.contains_key(name)),
        ))
    })
}

pub(super) fn add(
    path: &Path,
    providers: &toml::Table,
    skip_existing: bool,
) -> Result<Vec<String>, CliError> {
    transaction(path, |original, document| {
        let existing = document.get("providers").and_then(toml::Value::as_table);
        let mut additions = toml::Table::new();
        for (name, provider) in providers {
            if name.is_empty() || name.chars().any(char::is_control) || !provider.is_table() {
                return Err(error("Invalid provider declaration; file unchanged"));
            }
            if existing.is_some_and(|table| table.contains_key(name)) {
                if skip_existing {
                    continue;
                }
                return Err(error("Provider name already exists. Choose a different name or explicitly edit the existing provider; file unchanged"));
            }
            additions.insert(name.clone(), provider.clone());
        }
        let names = additions.keys().cloned().collect();
        if additions.is_empty() {
            return Ok((None, names));
        }
        Ok((Some(append_text(original, additions)?), names))
    })
}

pub(super) fn initialize(path: &Path, scaffold: &str) -> Result<bool, CliError> {
    transaction(path, |original, _| {
        if original.trim().is_empty() {
            Ok((Some(scaffold.into()), true))
        } else {
            Ok((None, false))
        }
    })
}

pub(super) fn default_from(
    path: &Path,
    name: &str,
    allow_empty_key: bool,
) -> Result<bool, CliError> {
    transaction(path, |original, document| {
        let Some(providers) = document.get("providers").and_then(toml::Value::as_table) else {
            return Ok((None, false));
        };
        if providers.contains_key("default") {
            return Ok((None, false));
        }
        let Some(provider) = providers.get(name).and_then(toml::Value::as_table) else {
            return Ok((None, false));
        };
        let key = provider
            .get("api_key")
            .and_then(toml::Value::as_str)
            .unwrap_or("");
        if !allow_empty_key && key.is_empty() {
            return Ok((None, false));
        }
        let type_ = provider
            .get("type")
            .and_then(toml::Value::as_str)
            .unwrap_or("openai");
        let mut defaults = toml::Table::new();
        defaults.insert("type".into(), toml::Value::String(type_.into()));
        defaults.insert("api_key".into(), toml::Value::String(key.into()));
        let mut additions = toml::Table::new();
        additions.insert("default".into(), toml::Value::Table(defaults));
        Ok((Some(append_text(original, additions)?), true))
    })
}
