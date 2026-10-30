use std::path::PathBuf;

pub const NAME: &str = "nanosuit";

pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_default().join(NAME)
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join(NAME)
}
