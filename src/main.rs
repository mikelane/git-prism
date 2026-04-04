mod git;
mod server;
mod tools;
mod treesitter;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "git-prism",
    version,
    about = "Agent-optimized git data MCP server"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio transport)
    Serve,
    /// Output a change manifest as JSON (CLI mode, no MCP)
    Manifest {
        /// Git ref range, e.g. "main..HEAD" or "abc1234"
        range: String,
        /// Path to the git repository (defaults to current directory)
        #[arg(long)]
        repo: Option<String>,
    },
    /// Output file snapshots as JSON (CLI mode, no MCP)
    Snapshot {
        /// Git ref range, e.g. "main..HEAD"
        range: String,
        /// File paths to snapshot
        #[arg(long, num_args = 1..)]
        paths: Vec<String>,
        /// Path to the git repository (defaults to current directory)
        #[arg(long)]
        repo: Option<String>,
    },
    /// List supported languages for function-level analysis
    Languages,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => {
            eprintln!("MCP server not yet implemented");
        }
        Commands::Manifest { range, repo } => {
            eprintln!("Manifest for {range} in {repo:?} — not yet implemented");
        }
        Commands::Snapshot { range, paths, repo } => {
            eprintln!("Snapshot for {range}, paths: {paths:?} in {repo:?} — not yet implemented");
        }
        Commands::Languages => {
            println!("Supported languages for function-level analysis:");
            println!("  go         (.go)");
            println!("  python     (.py)");
            println!("  typescript (.ts, .tsx)");
            println!("  javascript (.js, .jsx)");
            println!("  rust       (.rs)");
        }
    }

    Ok(())
}
