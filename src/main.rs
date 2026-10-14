use clap::{Args, Parser, Subcommand};
use hoard::config::{Config, ServeArgs, TokenPaths, drop_empty_hoard_env};

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
    let result = match cli.command {
        Command::Serve(args) | Command::Doctor(args) => Config::try_from(args).map(|_| ()),
        Command::Prune(p) => Config::try_from(p.serve).map(|_| ()),
        Command::Env(_) => Ok(()),
    };
    if let Err(e) = result {
        eprintln!("hoard: {e}");
        std::process::exit(1);
    }
    todo!("subcommand bodies land in later tasks");
}
