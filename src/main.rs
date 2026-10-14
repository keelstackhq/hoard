use std::io::IsTerminal;

use anyhow::bail;
use clap::{Args, Parser, Subcommand};
use hoard::config::{Config, ServeArgs, TokenPaths, drop_empty_hoard_env};
use hoard::doctor::run_doctor;
use hoard::serve::{env_for, run_serve};
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
    Serve(ServeArgs),
    /// Print export lines for a running server.
    Env(TokenPaths),
    /// Enforce max-size on R2 now.
    Prune(PruneArgs),
    /// Check storage credentials, bucket access and the R2 lifecycle rule.
    Doctor(ServeArgs),
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
        eprintln!("error: {e:#}");
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
        Command::Serve(args) => {
            let cfg = Config::try_from(args)?;
            init_logging();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(run_serve(cfg))
        }
        Command::Prune(p) => {
            Config::try_from(p.serve)?;
            let _ = p.force;
            bail!("`hoard prune` is not implemented yet")
        }
        Command::Doctor(args) => {
            let cfg = Config::try_from(args)?;
            init_logging();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(run_doctor(&cfg))
        }
    }
}
