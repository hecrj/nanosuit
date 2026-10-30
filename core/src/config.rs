use crate::environment;

use serde::Deserialize;

use std::fs;
use std::io;
use std::path::PathBuf;

/// The user's configuration, loaded from `config.toml` in the
/// configuration directory; a missing file is created holding the
/// template, so the configuration is discoverable on a fresh
/// machine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Config {
    pub sandbox: Sandbox,
}

impl Config {
    /// Loads the configuration from the file, creating it with the
    /// template when it is not there.
    pub fn load() -> io::Result<Self> {
        let file = Self::file();

        if !file.is_file() {
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent)?;
            }

            fs::write(&file, TEMPLATE)?;
        }

        let config = fs::read_to_string(file)?;

        toml::from_str(&config).map_err(io::Error::other)
    }

    pub(crate) fn file() -> PathBuf {
        environment::config_dir().join("config.toml")
    }
}

/// The sandbox policy: the extra mounts and environment the
/// bubblewrap sandbox gets on top of the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Sandbox {
    /// The host environment variables to forward into the sandbox,
    /// by name; the value is taken from the host at launch, so a
    /// variable the host has not set is skipped.
    #[serde(default)]
    pub env: Vec<String>,

    /// The paths to bring back into the sandbox read-only. A
    /// relative path, or one prefixed with `~/`, is resolved
    /// against the home directory; a path that is not on the host
    /// is skipped with a warning.
    #[serde(default)]
    pub read: Vec<PathBuf>,

    /// The paths to bring back into the sandbox writable; the
    /// resolution of the `read` field applies.
    #[serde(default)]
    pub write: Vec<PathBuf>,
}

/// The template written when the configuration file is created.
const TEMPLATE: &str = "\
# nanosuit's sandbox configuration.
#
# The bubblewrap sandbox binds the root read-only, with the project
# and the scratchpad /tmp as the writable places; these lists
# extend it. A relative path, or one prefixed with `~/`, is
# resolved against the home directory; a path that is not on the
# host is skipped with a warning.

[sandbox]
# The host environment variables to forward into the sandbox, by
# name; the value is taken from the host at launch, so a variable
# the host has not set is skipped.
env = []

# The paths to bring back read-only, like a public ssh key the
# model should be able to see.
read = []

# The paths to bring back writable, like a Rust toolchain and the
# user's projects directory, for building in them.
write = []
";
