//! Helpers shared by integration tests.
#![allow(dead_code)]

pub mod store_checks;

use hoard::config::{Config, LifecycleMode, Mode, Storage};

/// Config for the fs backend rooted under `root`.
pub fn fs_config(root: &std::path::Path) -> Config {
    Config {
        storage: Storage::Fs,
        mode: Mode::ReadWrite,
        cache_version: "v1".into(),
        port: 0,
        port_file: root.join("port"),
        token_file: root.join("token"),
        token: None,
        summary_file: None,
        fs_root: root.join("store"),
        r2: None,
        r2_lifecycle: LifecycleMode::Auto,
        max_age_days: 7,
        max_size: 1 << 30,
    }
}
