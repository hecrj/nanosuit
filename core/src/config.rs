use crate::Project;
use crate::environment;

use serde::Deserialize;

use std::fs;
use std::io;
use std::path::PathBuf;

/// The user's configuration, layered over two files: a global
/// `config.toml` in the configuration directory — a missing file
/// is created holding the template, so the configuration is
/// discoverable on a fresh machine — and an optional
/// `.nanosuit.toml` in the project directory. The project layer
/// only extends: its lists are appended after the global ones, so
/// it adds mounts and environment and never removes them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Config {
    pub sandbox: Sandbox,
}

impl Config {
    /// Loads the configuration from the global and the project
    /// files, creating the global one with the template when it is
    /// not there. A project file that fails to parse is skipped
    /// with a warning, so the global layer still applies.
    pub fn load(project: &Project) -> io::Result<Self> {
        let global: Self = {
            let file = Self::global_file();

            if !file.is_file() {
                if let Some(parent) = file.parent() {
                    fs::create_dir_all(parent)?;
                }

                fs::write(&file, TEMPLATE)?;
            }

            let config = fs::read_to_string(file)?;
            toml::from_str(&config).map_err(io::Error::other)?
        };

        let local = {
            let file = Self::local_file(project);

            if file.is_file() {
                let config = fs::read_to_string(file)?;

                toml::from_str(&config)
                    .inspect_err(|error| log::warn!("local configuration failed to load: {error}"))
                    .unwrap_or_default()
            } else {
                Self::default()
            }
        };

        Ok(global.merge(local))
    }

    pub(crate) fn global_file() -> PathBuf {
        environment::config_dir().join("config.toml")
    }

    pub(crate) fn local_file(project: &Project) -> PathBuf {
        project.join(".nanosuit.toml")
    }

    fn merge(self, other: Self) -> Self {
        Self {
            sandbox: self.sandbox.merge(other.sandbox),
        }
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

impl Sandbox {
    fn merge(mut self, mut other: Self) -> Self {
        self.env.append(&mut other.env);
        self.read.append(&mut other.read);
        self.write.append(&mut other.write);
        self
    }
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
#
# A project extends this configuration with a `.nanosuit.toml` in
# its own directory, holding the same tables; its lists are
# appended after these.

[sandbox]
# The host environment variables to forward into the sandbox, by
# name; the value is taken from the host at launch, so a variable
# the host has not set is skipped.
env = []

# The paths to bring back read-only, like `~/.ssh/known_hosts` and
# `~/.gitconfig` for git over ssh, or a public ssh key the model
# should be able to see.
read = []

# The paths to bring back writable, like a Rust toolchain and the
# user's projects directory, for building in them.
write = []
";

#[cfg(test)]
mod tests {
    use super::*;

    fn config(env: &[&str], read: &[&str], write: &[&str]) -> Config {
        Config {
            sandbox: Sandbox {
                env: env.iter().copied().map(String::from).collect(),
                read: read.iter().copied().map(PathBuf::from).collect(),
                write: write.iter().copied().map(PathBuf::from).collect(),
            },
        }
    }

    /// The project layer extends the global one: its lists are
    /// appended after the global ones, entry for entry.
    #[test]
    fn the_project_layer_extends_the_global_one() {
        let global = config(&["RUST_LOG"], &["~/.gitconfig"], &["~/.cargo"]);
        let local = config(&["NANOSUIT_TEST"], &["/etc/hosts"], &["~/code"]);

        assert_eq!(
            global.merge(local),
            config(
                &["RUST_LOG", "NANOSUIT_TEST"],
                &["~/.gitconfig", "/etc/hosts"],
                &["~/.cargo", "~/code"],
            )
        );
    }

    /// The project layer cannot shrink the global one: merging a
    /// default project layer — the outcome of a missing or broken
    /// file — leaves the global layer intact.
    #[test]
    fn the_project_layer_cannot_shrink_the_global_one() {
        let global = config(&["RUST_LOG"], &["~/.gitconfig"], &["~/.cargo"]);

        assert_eq!(global.clone().merge(Config::default()), global);
    }
}
