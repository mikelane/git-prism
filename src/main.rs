mod git;
mod server;
mod tools;
mod treesitter;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use tools::{ManifestOptions, SnapshotOptions, build_manifest, build_snapshots};

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

fn parse_range(range: &str) -> (&str, &str) {
    if let Some((base, head)) = range.split_once("..") {
        (base, head)
    } else {
        (range, "HEAD")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_range_with_double_dot() {
        let (base, head) = parse_range("main..HEAD");
        assert_eq!(base, "main");
        assert_eq!(head, "HEAD");
    }

    #[test]
    fn it_parses_bare_ref_as_base_with_head_default() {
        let (base, head) = parse_range("abc1234");
        assert_eq!(base, "abc1234");
        assert_eq!(head, "HEAD");
    }

    #[test]
    fn it_parses_head_tilde_range() {
        let (base, head) = parse_range("HEAD~3..HEAD");
        assert_eq!(base, "HEAD~3");
        assert_eq!(head, "HEAD");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => {
            server::run_server().await?;
        }
        Commands::Manifest { range, repo } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let (base_ref, head_ref) = parse_range(&range);
            let options = ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis: true,
            };
            let manifest = build_manifest(&repo_path, base_ref, head_ref, &options)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Commands::Snapshot { range, paths, repo } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let (base_ref, head_ref) = parse_range(&range);
            let options = SnapshotOptions {
                include_before: true,
                include_after: true,
                max_file_size_bytes: 100_000,
                line_range: None,
            };
            let snapshots = build_snapshots(&repo_path, base_ref, head_ref, &paths, &options)?;
            println!("{}", serde_json::to_string_pretty(&snapshots)?);
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
