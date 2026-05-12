//! Filesystem convention helpers — where to find the config file and
//! where install / generate stage their working tree.

use std::path::{Path, PathBuf};

use super::CONFIG_FILE_NAME;

/// Convenience for callers that have a directory and want to find the
/// canonical config file inside it.
pub fn config_path_in(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE_NAME)
}

/// Staging directory convention: `<config-dir>/.move-bindgen/<name>/`.
///
/// All install artefacts for a given directory live under one
/// `.move-bindgen/` root, with a per-config subdirectory keyed by the
/// config's file stem. Examples:
///
/// - `configs/exchange.toml` → `configs/.move-bindgen/exchange/`
/// - `configs/pyth.toml`     → `configs/.move-bindgen/pyth/`
/// - `./move-bindgen.toml`   → `./.move-bindgen/default/`
///
/// The canonical `move-bindgen.toml` filename maps to `default/`
/// rather than the literal `move-bindgen/` to avoid the awkward
/// `.move-bindgen/move-bindgen/` doubled name.
///
/// One root means one gitignore line (`/.move-bindgen/`) regardless
/// of how many configs share a directory.
pub fn staging_dir_for(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = config_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default");
    let subdir = if stem == "move-bindgen" {
        "default"
    } else {
        stem
    };
    dir.join(".move-bindgen").join(subdir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_dir_basics() {
        assert_eq!(
            staging_dir_for(Path::new("configs/exchange.toml")),
            PathBuf::from("configs/.move-bindgen/exchange"),
        );
        assert_eq!(
            staging_dir_for(Path::new("configs/counter.toml")),
            PathBuf::from("configs/.move-bindgen/counter"),
        );
        assert_eq!(
            staging_dir_for(Path::new("./move-bindgen.toml")),
            PathBuf::from("./.move-bindgen/default"),
        );
    }

    #[test]
    fn multiple_configs_in_one_dir_share_a_root() {
        // Two siblings under the same `.move-bindgen/`. This is the
        // whole point of the layout — one dotfile to gitignore.
        let a = staging_dir_for(Path::new("configs/exchange.toml"));
        let b = staging_dir_for(Path::new("configs/pyth.toml"));
        assert_eq!(a.parent(), b.parent());
        assert_eq!(a.parent().unwrap(), Path::new("configs/.move-bindgen"));
    }
}
