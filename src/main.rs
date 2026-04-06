mod git;
mod server;
mod telemetry;
mod tools;
mod treesitter;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use tools::{
    ManifestOptions, SnapshotOptions, build_history, build_manifest, build_snapshots,
    build_worktree_manifest,
};

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
    /// Output per-commit history manifests as JSON (CLI mode, no MCP)
    History {
        /// Git ref range, e.g. "HEAD~3..HEAD"
        range: String,
        /// Path to the git repository (defaults to current directory)
        #[arg(long)]
        repo: Option<String>,
    },
    /// List supported languages for function-level analysis
    Languages,
}

enum RefRange<'a> {
    /// A range between two refs (e.g. "main..HEAD", "HEAD~1..HEAD")
    CommitRange { base: &'a str, head: &'a str },
    /// A single ref compared against the working tree (e.g. "HEAD")
    WorktreeCompare { base: &'a str },
}

fn validate_commit_range(range: &RefRange<'_>, subcommand: &str) -> anyhow::Result<()> {
    match range {
        RefRange::WorktreeCompare { .. } => {
            anyhow::bail!(
                "{subcommand} does not support working tree mode — use a commit range (e.g., HEAD~1..HEAD)"
            )
        }
        RefRange::CommitRange { .. } => Ok(()),
    }
}

fn parse_range(range: &str) -> RefRange<'_> {
    if let Some((base, head)) = range.split_once("...") {
        RefRange::CommitRange {
            base,
            head: if head.is_empty() { "HEAD" } else { head },
        }
    } else if let Some((base, head)) = range.split_once("..") {
        RefRange::CommitRange {
            base,
            head: if head.is_empty() { "HEAD" } else { head },
        }
    } else {
        RefRange::WorktreeCompare { base: range }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_range_with_double_dot() {
        let result = parse_range("main..HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_bare_ref_as_worktree_compare() {
        let result = parse_range("abc1234");
        assert!(matches!(
            result,
            RefRange::WorktreeCompare { base: "abc1234" }
        ));
    }

    #[test]
    fn it_parses_head_as_worktree_compare() {
        let result = parse_range("HEAD");
        assert!(matches!(result, RefRange::WorktreeCompare { base: "HEAD" }));
    }

    #[test]
    fn it_parses_head_tilde_range() {
        let result = parse_range("HEAD~3..HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "HEAD~3",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_three_dot_range() {
        let result = parse_range("main...HEAD");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_parses_three_dot_range_with_empty_head_as_head() {
        let result = parse_range("main...");
        assert!(matches!(
            result,
            RefRange::CommitRange {
                base: "main",
                head: "HEAD"
            }
        ));
    }

    #[test]
    fn it_rejects_worktree_mode_for_history_command() {
        let range = "HEAD";
        let ref_range = parse_range(range);
        let err = validate_commit_range(&ref_range, "history");
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("does not support working tree mode"),
            "expected 'does not support working tree mode' in: {msg}"
        );
    }

    #[test]
    fn it_accepts_commit_range_for_history_command() {
        let range = "HEAD~3..HEAD";
        let ref_range = parse_range(range);
        let result = validate_commit_range(&ref_range, "history");
        assert!(result.is_ok());
    }

    #[test]
    fn it_rejects_worktree_mode_for_snapshot_command() {
        let range = "HEAD";
        let ref_range = parse_range(range);
        let err = validate_commit_range(&ref_range, "snapshot");
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("does not support working tree mode"),
            "expected 'does not support working tree mode' in: {msg}"
        );
    }

    #[test]
    fn it_accepts_commit_range_for_snapshot_command() {
        let range = "HEAD~1..HEAD";
        let ref_range = parse_range(range);
        let result = validate_commit_range(&ref_range, "snapshot");
        assert!(result.is_ok());
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
            let options = ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis: true,
            };
            let manifest = match parse_range(&range) {
                RefRange::CommitRange { base, head } => {
                    build_manifest(&repo_path, base, head, &options)?
                }
                RefRange::WorktreeCompare { base } => {
                    build_worktree_manifest(&repo_path, base, &options)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Commands::History { range, repo } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let ref_range = parse_range(&range);
            validate_commit_range(&ref_range, "history")?;
            let (base_ref, head_ref) = match ref_range {
                RefRange::CommitRange { base, head } => (base, head),
                RefRange::WorktreeCompare { .. } => unreachable!("validated above"),
            };
            let options = ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis: true,
            };
            let history = build_history(&repo_path, base_ref, head_ref, &options)?;
            println!("{}", serde_json::to_string_pretty(&history)?);
        }
        Commands::Snapshot { range, paths, repo } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let ref_range = parse_range(&range);
            validate_commit_range(&ref_range, "snapshot")?;
            let (base_ref, head_ref) = match ref_range {
                RefRange::CommitRange { base, head } => (base, head),
                RefRange::WorktreeCompare { .. } => unreachable!("validated above"),
            };
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
            println!("  c          (.c, .h)");
            println!("  cpp        (.cpp, .hpp, .cc, .cxx, .hh, .hxx)");
            println!("  go         (.go)");
            println!("  java       (.java)");
            println!("  javascript (.js, .jsx)");
            println!("  python     (.py)");
            println!("  rust       (.rs)");
            println!("  typescript (.ts, .tsx)");
        }
    }

    Ok(())
}
