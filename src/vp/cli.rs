//! `hoard vp restore` / `hoard vp save`: flags, setup and the one line of output.
//!
//! Setup errors (bad flags, missing GitHub env) fail the command. Everything after setup, a
//! storage or disk error included, prints one line and exits 0: the cache never fails the
//! build.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, Subcommand};

use super::{
    GhaTwirp, RestoreOutcome, SaveOutcome, VpBackend, default_os_arch, default_run_key,
    vp_cache_dir, vp_restore, vp_save,
};
use crate::config::{Config, Mode, ServeArgs, Storage, parse_size, valid_segment};
use crate::redact::redacted;
use crate::serve::storage_layout;
use crate::stats::human_bytes;
use crate::store::{Store, build_operator};

#[derive(Subcommand)]
pub enum VpCommand {
    /// Restore the newest Vite+ task cache for this OS and architecture.
    Restore(VpArgs),
    /// Save the Vite+ task cache of this run.
    Save(VpArgs),
}

#[derive(Args)]
pub struct VpArgs {
    #[command(flatten)]
    pub serve: ServeArgs,
    /// Workspace root; the task cache is node_modules/.vite/task-cache under it, or
    /// VITE_CACHE_PATH when that is absolute.
    #[arg(long, env = "HOARD_WORKSPACE", default_value = ".")]
    pub workspace: PathBuf,
    /// Key segment for the OS and architecture, e.g. linux-x64 (default: this machine).
    #[arg(long, env = "HOARD_OS_ARCH")]
    pub os_arch: Option<String>,
    /// Key segment of this run's archive (default: GITHUB_RUN_ID-GITHUB_RUN_ATTEMPT).
    #[arg(long, env = "HOARD_RUN_KEY")]
    pub run_key: Option<String>,
    /// Skip the save when the task cache is larger: KB, MB, GB or TB (powers of 1000).
    #[arg(long, env = "HOARD_VP_MAX_SIZE", default_value = "2GB")]
    pub vp_max_size: String,
}

/// One key segment, else a setup error naming the flag and its env var.
fn segment(value: String, flag: &str, env: &str) -> anyhow::Result<String> {
    if valid_segment(&value) && value != super::POINTER {
        return Ok(value);
    }
    anyhow::bail!(
        "{flag} {value:?} must be one segment of A-Z a-z 0-9 . _ - (not ., .. or latest); \
         set --{flag} or {env}"
    )
}

/// A validated `vp` command, ready to run.
pub struct VpJob {
    save: bool,
    cfg: Config,
    dir: PathBuf,
    os_arch: String,
    run_key: String,
    max_size: u64,
}

/// Validates `cmd`: a bad flag is a setup error.
pub fn prepare(cmd: VpCommand) -> anyhow::Result<VpJob> {
    let (save, a) = match cmd {
        VpCommand::Restore(a) => (false, a),
        VpCommand::Save(a) => (true, a),
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    let lookup = |k: &str| env.get(k).cloned();
    let os_arch = segment(
        a.os_arch.unwrap_or_else(default_os_arch),
        "os-arch",
        "HOARD_OS_ARCH",
    )?;
    let run_key = segment(
        a.run_key.unwrap_or_else(|| default_run_key(lookup)),
        "run-key",
        "HOARD_RUN_KEY",
    )?;
    let max_size = parse_size(&a.vp_max_size)?;
    let dir = vp_cache_dir(&a.workspace, &env);
    Ok(VpJob {
        save,
        cfg: Config::try_from(a.serve)?,
        dir,
        os_arch,
        run_key,
        max_size,
    })
}

fn backend(cfg: &Config) -> anyhow::Result<VpBackend> {
    if cfg.storage == Storage::Github {
        return Ok(VpBackend::Github(GhaTwirp::from_env(|k| {
            std::env::var(k).ok()
        })?));
    }
    let (prefix, _) = storage_layout(cfg)?;
    Ok(VpBackend::Store {
        store: Arc::new(Store::new(build_operator(cfg)?)),
        prefix,
    })
}

impl VpJob {
    /// Builds the backend (setup errors are `Err`) and runs the command; the `Ok` line covers
    /// every outcome after setup, storage errors included.
    pub async fn run(self) -> anyhow::Result<String> {
        let j = self;
        let backend = backend(&j.cfg)?;
        let cv = &j.cfg.cache_version;
        if j.cfg.mode == Mode::Off {
            return Ok("skipped: mode=off".into());
        }
        if j.save {
            if j.cfg.mode == Mode::Read {
                return Ok("skipped: mode=read (nothing is written)".into());
            }
            let r = vp_save(&backend, cv, &j.os_arch, &j.run_key, &j.dir, j.max_size).await;
            Ok(match r {
                Ok(SaveOutcome::Saved { key, bytes }) => {
                    format!("saved {key} ({})", human_bytes(bytes))
                }
                Ok(SaveOutcome::SkippedTooLarge { bytes }) => format!(
                    "skipped: the task cache is {}, larger than vp-max-size ({}); run `vp cache clean`",
                    human_bytes(bytes),
                    human_bytes(j.max_size)
                ),
                Ok(SaveOutcome::SkippedEmpty) => {
                    "skipped: cache directory is empty or missing".into()
                }
                Err(e) => format!(
                    "warning: Vite+ cache save failed ({}); the build continues without it",
                    redacted(&e)
                ),
            })
        } else {
            if j.cfg.no_reads {
                return Ok("skipped: no-reads (the cache is not read)".into());
            }
            Ok(match vp_restore(&backend, cv, &j.os_arch, &j.dir).await {
                Ok(RestoreOutcome::Restored { key, bytes }) => {
                    format!("restored {key} ({})", human_bytes(bytes))
                }
                Ok(RestoreOutcome::NotFound) => "no cache found".into(),
                Err(e) => format!(
                    "warning: Vite+ cache restore failed ({}); the build continues without it",
                    redacted(&e)
                ),
            })
        }
    }
}
