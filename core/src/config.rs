use crate::Project;
use crate::environment;

use indexmap::IndexMap;
use serde::Deserialize;

use std::fs;
use std::io;
use std::path::PathBuf;

/// The user's configuration, layered over two files: a global
/// `config.toml` in the configuration directory — a missing file
/// is created holding the template, so the configuration is
/// discoverable on a fresh machine — and an optional
/// `.nanosuit.toml` in the project directory. The project layer
/// extends: its lists are appended after the global ones, so it
/// adds mounts; and its `env` entries override the global ones by
/// name, so a project can pin a variable to a literal value, or
/// set one to `false` to stop forwarding it.
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
    /// The environment variables the sandbox gets, by name. The
    /// list form forwards the host's value for each name at
    /// launch, so a variable the host has not set is skipped; the
    /// table form pins a literal value — like `env = { EDITOR =
    /// "nvim" }` — forwards the host's value for a `true` entry,
    /// and skips a `false` one.
    #[serde(default)]
    pub env: Env,

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

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(from = "_Env")]
pub struct Env(IndexMap<String, Binding>);

impl Env {
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Binding)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    fn merge(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }
}

impl FromIterator<String> for Env {
    fn from_iter<I: IntoIterator<Item = String>>(it: I) -> Self {
        Self(
            it.into_iter()
                .map(|variable| (variable, Binding::Inherit(true)))
                .collect(),
        )
    }
}

impl FromIterator<(String, Binding)> for Env {
    fn from_iter<I: IntoIterator<Item = (String, Binding)>>(it: I) -> Self {
        Self(it.into_iter().collect())
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum _Env {
    List(Vec<String>),
    Map(IndexMap<String, Binding>),
}

impl From<_Env> for Env {
    fn from(env: _Env) -> Self {
        match env {
            _Env::List(items) => Self(
                items
                    .into_iter()
                    .map(|variable| (variable, Binding::Inherit(true)))
                    .collect(),
            ),
            _Env::Map(map) => Self(map),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    Value(String),
    Inherit(bool),
}

impl Sandbox {
    fn merge(mut self, mut other: Self) -> Self {
        self.env = self.env.merge(other.env);
        self.read.append(&mut other.read);
        self.write.append(&mut other.write);
        self
    }
}

/// The template written when the configuration file is created.
const TEMPLATE: &str = r#"# nanosuit sandbox configuration
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
# The environment variables the sandbox gets, by name. The list
# form forwards the host's value at launch, so a variable the host
# has not set is skipped:
#
#     env = ["RUST_LOG"]
#
# The table form pins a literal value for a string entry,
# forwards the host's value for a `true` entry, and skips a
# `false` one:
#
#     env = { EDITOR = "nvim", RUST_LOG = true, OTHER = false }
env = []

# The paths to bring back read-only, like `~/.ssh/known_hosts` and
# `~/.gitconfig` for git over ssh, or a public ssh key the model
# should be able to see.
read = []

# The paths to bring back writable, like a Rust toolchain and the
# user's projects directory, for building in them.
write = []
"#;

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

    /// The `env` field reads the list form, in order: each name
    /// forwards the host's value at launch.
    #[test]
    fn the_env_reads_the_list_form() {
        let config: Config =
            toml::from_str("[sandbox]\nenv = [\"RUST_LOG\", \"TOKEN\"]\n").unwrap();

        let entries: Vec<_> = config.sandbox.env.iter().collect();

        assert_eq!(
            entries,
            [
                ("RUST_LOG", &Binding::Inherit(true)),
                ("TOKEN", &Binding::Inherit(true)),
            ]
        );
    }

    /// The `env` field reads the table form, in the order written:
    /// a string pins a literal value, `true` forwards the host's
    /// value, and `false` skips the variable.
    #[test]
    fn the_env_reads_the_table_form() {
        let config: Config = toml::from_str(
            "[sandbox]\nenv = { TOKEN = \"secret\", RUST_LOG = true, NANOSUIT_TEST = false }\n",
        )
        .unwrap();

        let entries: Vec<_> = config.sandbox.env.iter().collect();

        assert_eq!(
            entries,
            [
                ("TOKEN", &Binding::Value("secret".to_owned())),
                ("RUST_LOG", &Binding::Inherit(true)),
                ("NANOSUIT_TEST", &Binding::Inherit(false)),
            ]
        );
    }

    /// An `env` entry of a type other than string or boolean is
    /// rejected.
    #[test]
    fn an_env_entry_of_a_foreign_type_is_rejected() {
        assert!(toml::from_str::<Config>("[sandbox]\nenv = { NANOSUIT_TEST = 1 }\n").is_err());
    }

    /// The project layer's `env` entries override the global ones
    /// by name — a pinned value beats the inherited one, and
    /// `false` stops a variable the global layer forwards — while
    /// the global entries keep their places and the new ones
    /// follow.
    #[test]
    fn the_project_layer_overrides_the_global_env() {
        let global = config(&["RUST_LOG", "TOKEN"], &[], &[]);

        let local: Config =
            toml::from_str("[sandbox]\nenv = { RUST_LOG = \"trace\", NANOSUIT_TEST = false }\n")
                .unwrap();

        let merged = global.merge(local);
        let entries: Vec<_> = merged.sandbox.env.iter().collect();

        assert_eq!(
            entries,
            [
                ("RUST_LOG", &Binding::Value("trace".to_owned())),
                ("TOKEN", &Binding::Inherit(true)),
                ("NANOSUIT_TEST", &Binding::Inherit(false)),
            ]
        );
    }

    /// The template written on a fresh machine parses, and holds
    /// nothing but the defaults.
    #[test]
    fn the_template_parses_as_the_default_configuration() {
        assert_eq!(
            toml::from_str::<Config>(TEMPLATE).unwrap(),
            Config::default()
        );
    }
}
