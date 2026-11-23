//! Where Atuin keeps its files.
//!
//! The agent shares Atuin's config and data directories: its settings are `ai.toml` beside
//! Atuin's `config.toml`, and it reads Atuin's history database. These rules mirror
//! `atuin_common::dirs` in atuinsh/atuin, minus the `themes` special cases; when Atuin changes
//! where it looks, this module has to follow.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Error returned when the user's home directory can't be determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("could not determine home directory")]
pub struct HomeError;

/// Get the user's home directory, e.g. `/home/ellie`.
pub fn try_home_dir() -> Result<PathBuf, HomeError> {
    ::dirs::home_dir().ok_or(HomeError)
}

/// Atuin's home directory: `$ATUIN_HOME`, or `~/.atuin`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AtuinHome {
    /// The path came from `ATUIN_HOME`, which was nonempty and absolute.
    Set(PathBuf),
    /// `ATUIN_HOME` was unset, so this is the default `~/.atuin`.
    Default(PathBuf),
}

impl AtuinHome {
    fn into_path(self) -> PathBuf {
        match self {
            Self::Set(path) | Self::Default(path) => path,
        }
    }
}

fn try_atuin_home() -> Result<AtuinHome, HomeError> {
    if let Some(value) = var_abspath("ATUIN_HOME") {
        return Ok(AtuinHome::Set(value));
    }
    Ok(AtuinHome::Default(try_home_dir()?.join(".atuin")))
}

/// Check whether `ATUIN_HOME` is set to an absolute path.
#[must_use]
pub fn atuin_home_is_set() -> bool {
    var_abspath("ATUIN_HOME").is_some()
}

/// Top-level config items affected by the `ATUIN_CONFIG_DIR` environment variable: Atuin's are
/// `config.toml` and `server.toml`, and `ai.toml` sits beside them.
const ATUIN_CONFIG_DIR_ITEMS: [&str; 1] = ["ai.toml"];

/// Get the path of a config item, such as `ai.toml` or `skills`.
///
/// `$ATUIN_CONFIG_DIR` wins for `ai.toml`, then `$ATUIN_HOME`, then whichever of `~/.atuin`
/// and the legacy `$XDG_CONFIG_HOME/atuin` already holds the item, then `~/.atuin`.
///
/// # Panics
///
/// Panics if `child` is empty or absolute, or if it starts with `.` or `..`. Also panics if the
/// user's home directory can't be determined and the path can't be calculated without it.
#[must_use]
pub fn config_path(child: impl AsRef<Path>) -> PathBuf {
    let child = child.as_ref();
    let Some(std::path::Component::Normal(top)) = child.components().next() else {
        panic!("config path must be relative and start with a top-level item: {child:?}");
    };

    if ATUIN_CONFIG_DIR_ITEMS.iter().any(|name| *name == top)
        && let Some(value) = var_nonempty("ATUIN_CONFIG_DIR")
    {
        return PathBuf::from(value).join(child);
    }

    let home = match try_atuin_home() {
        Ok(AtuinHome::Set(path)) => return path.join(child),
        Ok(AtuinHome::Default(path)) => Ok(path),
        Err(e) => Err(e),
    };

    if let Ok(home) = &home
        && home.join(top).exists()
    {
        return home.join(child);
    }

    // Atuin used to keep config items in `$XDG_CONFIG_HOME/atuin`, and still reads them from
    // there when they don't exist in `~/.atuin`.
    if let Ok(legacy) = legacy_config_dir()
        && legacy.join(top).exists()
    {
        return legacy.join(child);
    }

    // An `ai.toml` in `~/.config/atuin` keeps winning even when `XDG_CONFIG_HOME` points
    // elsewhere, so `/model` doesn't create a `~/.atuin` one that would shadow it.
    if ATUIN_CONFIG_DIR_ITEMS.iter().any(|name| *name == top)
        && legacy_config_dir_with(XdgDir::Default).is_ok_and(|dir| dir.join(top).exists())
    {
        return legacy_config_dir().unwrap_or_else(|err| panic!("{err}")).join(child);
    }

    home.unwrap_or_else(|err| panic!("{err}")).join(child)
}

/// An XDG Base Directory.
enum XdgDir {
    /// The directory has been explicitly set via an environment variable.
    Explicit(PathBuf),
    /// The default value of the XDG Base Directory should be used.
    Default,
}

impl XdgDir {
    fn from_var(name: &str) -> Self {
        match var_abspath(name) {
            Some(path) => Self::Explicit(path),
            None => Self::Default,
        }
    }

    /// Both the custom XDG dir in the environment variable `name`, and the default dir when `name`
    /// is set.
    fn with_default(name: &str) -> impl Iterator<Item = Self> {
        let dir = Self::from_var(name);
        let default = matches!(dir, Self::Explicit(_)).then_some(Self::Default);
        std::iter::once(dir).chain(default)
    }
}

/// `$XDG_CONFIG_HOME/atuin`, or `~/.config/atuin` if unset: where Atuin kept its config before
/// `~/.atuin`.
fn legacy_config_dir() -> Result<PathBuf, HomeError> {
    legacy_config_dir_with(XdgDir::from_var("XDG_CONFIG_HOME"))
}

fn legacy_config_dir_with(xdg: XdgDir) -> Result<PathBuf, HomeError> {
    let path = match xdg {
        XdgDir::Explicit(path) => path,
        XdgDir::Default => try_home_dir()?.join(".config"),
    };
    Ok(path.join("atuin"))
}

/// Atuin's data directory, unless its `config.toml` sets `data_dir`.
///
/// This is `$ATUIN_HOME/data`, falling back to [`legacy_data_dir`] when `ATUIN_HOME` is unset,
/// `~/.atuin/data` doesn't exist, and a legacy data dir does.
///
/// # Panics
///
/// Panics if `ATUIN_HOME` is unset and the user's home directory can't be determined.
#[must_use]
pub fn unconfigured_data_dir() -> PathBuf {
    match try_atuin_home().unwrap_or_else(|err| panic!("{err}")) {
        AtuinHome::Set(path) => path.join("data"),
        AtuinHome::Default(path) => {
            let path = path.join("data");
            if !path.exists() && existing_legacy_data_dirs().next().is_some() {
                return legacy_data_dir();
            }
            path
        }
    }
}

/// Existing legacy data dirs: [`legacy_data_dir`], or `~/.local/share/atuin` even when
/// `XDG_DATA_HOME` points elsewhere.
fn existing_legacy_data_dirs() -> impl Iterator<Item = PathBuf> {
    XdgDir::with_default("XDG_DATA_HOME")
        .filter_map(|xdg| legacy_data_dir_with(xdg).ok())
        .filter(|dir| dir.exists())
}

/// `$XDG_DATA_HOME/atuin`, or `~/.local/share/atuin` if unset: where Atuin kept its data before
/// `~/.atuin`.
///
/// # Panics
///
/// Panics if `XDG_DATA_HOME` is unset and the user's home directory can't be determined.
#[must_use]
pub fn legacy_data_dir() -> PathBuf {
    legacy_data_dir_with(XdgDir::from_var("XDG_DATA_HOME")).unwrap_or_else(|err| panic!("{err}"))
}

fn legacy_data_dir_with(xdg: XdgDir) -> Result<PathBuf, HomeError> {
    let path = match xdg {
        XdgDir::Explicit(path) => path,
        XdgDir::Default => try_home_dir()?.join(".local").join("share"),
    };
    Ok(path.join("atuin"))
}

/// `$ATUIN_HOME/logs`.
///
/// # Panics
///
/// Panics if `ATUIN_HOME` is unset and the user's home directory can't be determined.
#[must_use]
pub fn logs_dir() -> PathBuf {
    try_atuin_home().unwrap_or_else(|err| panic!("{err}")).into_path().join("logs")
}

/// The variable `name`, unless it is unset or empty.
fn var_nonempty(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// The variable `name` as a path, unless it is unset, empty or relative: a relative directory
/// would move with the working directory.
fn var_abspath(name: &str) -> Option<PathBuf> {
    var_nonempty(name).map(PathBuf::from).filter(|path| path.is_absolute())
}
