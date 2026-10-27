use std::io::IsTerminal;

use clap::{Args, Parser, Subcommand};
use hoard::config::{Config, ServeArgs, TokenPaths, drop_empty_hoard_env};
use hoard::doctor::run_doctor;
use hoard::r2::prune::run_prune;
use hoard::redact::redacted;
use hoard::serve::{env_for, run_serve};
use hoard::standalone::{DetachArgs, StopArgs, detach, stop};
use hoard::vp::cli::{VpCommand, prepare};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hoard",
    version,
    about = "CI cache server for Turborepo and Vite+"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the cache server until shutdown.
    Serve {
        #[command(flatten)]
        serve: ServeArgs,
        #[command(flatten)]
        detach: DetachArgs,
    },
    /// Print export lines for a running server.
    Env(TokenPaths),
    /// Shut down a running server, wait for its uploads, print the summary.
    Stop(StopArgs),
    /// Enforce max-size on R2 now.
    Prune(PruneArgs),
    /// Check storage credentials, bucket access and the R2 lifecycle rule.
    Doctor(ServeArgs),
    /// Restore or save the Vite+ task-cache directory.
    #[command(subcommand)]
    Vp(VpCommand),
}

#[derive(Args)]
struct PruneArgs {
    /// Ignore the 24 h throttle.
    #[arg(long)]
    force: bool,
    #[command(flatten)]
    serve: ServeArgs,
}

fn main() {
    drop_empty_hoard_env();
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {}", redacted(&e));
        std::process::exit(1);
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Env(paths) => {
            print!("{}", env_for(&paths)?);
            Ok(())
        }
        Command::Stop(a) => stop(&a),
        Command::Serve {
            serve: args,
            detach: d,
        } => {
            let pack_ignored = args.storage == hoard::config::Storage::R2
                && args.pack == hoard::config::PackMode::On;
            let cfg = Config::try_from(args)?;
            if d.detach {
                return detach(&cfg, &d);
            }
            init_logging();
            if pack_ignored {
                tracing::info!(
                    "pack is ignored for storage=r2: R2 has no upload rate limit; artifacts are stored one by one"
                );
            }
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(run_serve(cfg))
        }
        Command::Prune(p) => {
            let cfg = Config::try_from(p.serve)?;
            init_logging();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(run_prune(&cfg, p.force))
        }
        Command::Doctor(args) => {
            let cfg = Config::try_from(args)?;
            init_logging();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(run_doctor(&cfg))
        }
        Command::Vp(cmd) => {
            let job = prepare(cmd)?;
            init_logging();
            let line = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(job.run())?;
            println!("{line}");
            Ok(())
        }
    }
}
