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
use crate::serve::{storage_layout, termination_signal};
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
    /// Key segment for the OS and architecture: linux-x64, linux-arm64, darwin-x64,
    /// darwin-arm64 or windows-x64 (default: this machine).
    #[arg(long, env = "HOARD_OS_ARCH")]
    pub os_arch: Option<String>,
    /// Key segment of this run's archive (default: GITHUB_RUN_ID-GITHUB_RUN_ATTEMPT-<8 random
    /// hex>, else local-<unix secs>-<8 random hex>).
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
    /// Runs the command and returns its one line. The mode skips come first, before any storage
    /// setup; a setup error (backend, signal handler) is `Err`; every other outcome, a storage
    /// error or SIGTERM / Ctrl-C included, is an `Ok` line. An interrupted command drops its
    /// work, which removes the spooled archive and a half-extracted directory.
    pub async fn run(self) -> anyhow::Result<String> {
        let j = self;
        let cmd = if j.save { "save" } else { "restore" };
        if j.cfg.mode == Mode::Off {
            return Ok("skipped: mode=off".into());
        }
        if j.save && j.cfg.mode == Mode::Read {
            return Ok("skipped: mode=read (nothing is written)".into());
        }
        if !j.save && j.cfg.no_reads {
            return Ok("skipped: no-reads (the cache is not read)".into());
        }
        let backend = backend(&j.cfg)?;
        let signal = termination_signal()?;
        let work = async {
            let cv = &j.cfg.cache_version;
            if j.save {
                vp_save(&backend, cv, &j.os_arch, &j.run_key, &j.dir, j.max_size)
                    .await
                    .map(|o| match o {
                        SaveOutcome::Saved { key, bytes } => {
                            format!("saved {key} ({})", human_bytes(bytes))
                        }
                        SaveOutcome::SkippedTooLarge { bytes } => format!(
                            "skipped: the task cache is {}, larger than vp-max-size ({}); run `vp cache clean`",
                            human_bytes(bytes),
                            human_bytes(j.max_size)
                        ),
                        SaveOutcome::SkippedEmpty => {
                            "skipped: cache directory is empty or missing".into()
                        }
                    })
            } else {
                vp_restore(&backend, cv, &j.os_arch, &j.dir)
                    .await
                    .map(|o| match o {
                        RestoreOutcome::Restored { key, bytes } => {
                            format!("restored {key} ({})", human_bytes(bytes))
                        }
                        RestoreOutcome::NotFound => "no cache found".into(),
                    })
            }
        };
        let result = tokio::select! {
            r = work => r,
            _ = signal => Err(anyhow::anyhow!("interrupted")),
        };
        Ok(result.unwrap_or_else(|e| {
            format!(
                "warning: Vite+ cache {cmd} failed ({}); the build continues without it",
                redacted(&e)
            )
        }))
    }
}
