mod git;
mod hooks;
pub(crate) mod metrics;
pub(crate) mod pagination;
pub(crate) mod privacy;
mod server;
mod telemetry;
mod tools;
mod treesitter;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use tools::{
    ContextOptions, FunctionContextResponse, ManifestOptions, SnapshotOptions,
    build_function_context_with_options, build_snapshots, collect_all_history_pages,
    collect_all_manifest_pages, collect_all_worktree_manifest_pages,
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
        /// Page size for internal pagination (default 500)
        #[arg(long, default_value_t = 500)]
        page_size: usize,
        /// Include function-level analysis (off by default)
        #[arg(long)]
        include_function_analysis: bool,
        /// Maximum estimated tokens for the response (default 8192, 0 to disable)
        #[arg(long, default_value_t = 8192)]
        max_response_tokens: usize,
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
        /// Page size for internal pagination (default 500)
        #[arg(long, default_value_t = 500)]
        page_size: usize,
    },
    /// Output function context (callers, callees, test references) as JSON
    Context {
        /// Git ref range, e.g. "HEAD~1..HEAD"
        range: String,
        /// Path to the git repository (defaults to current directory)
        #[arg(long)]
        repo: Option<String>,
        /// Opaque pagination cursor from a previous response.
        #[arg(long)]
        cursor: Option<String>,
        /// Maximum functions per page (1–500, default 25).
        #[arg(long, default_value_t = 25)]
        page_size: usize,
        /// Comma-separated list of function names to scope the response to.
        #[arg(long)]
        function_names: Option<String>,
        /// Maximum estimated tokens for the response (default 8192, 0 to disable).
        #[arg(long, default_value_t = 8192)]
        max_response_tokens: usize,
    },
    /// List supported languages for function-level analysis
    Languages,
    /// Install / uninstall / report status of the bundled redirect hook
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install the bundled redirect hook into Claude Code's settings
    Install {
        /// Where to install the hook
        #[arg(long, value_parser = ["user", "project", "local"])]
        scope: String,
        /// Print the would-be settings JSON without writing anything
        #[arg(long)]
        dry_run: bool,
        /// Overwrite a user-edited entry in place
        #[arg(long)]
        force: bool,
    },
    /// Remove redirect-hook entries written by this binary
    Uninstall {
        /// Which scope to clean up
        #[arg(long, value_parser = ["user", "project", "local"])]
        scope: String,
    },
    /// Report which scopes have the redirect hook installed
    Status,
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

/// Dispatch a `git-prism hooks <subcommand>` invocation. Returns the exit
/// code the process should adopt — 0 on success, non-zero when the
/// subcommand needs to surface an error to the shell (e.g. v2 -> v1
/// downgrade refusal).
fn run_hooks_command(command: HooksCommands) -> anyhow::Result<i32> {
    let home = hooks::home_dir()?;
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cannot determine current directory: {e}"))?;
    match command {
        HooksCommands::Install {
            scope,
            dry_run,
            force,
        } => {
            let scope = hooks::Scope::parse(&scope)?;
            let options = hooks::InstallOptions {
                scope,
                dry_run,
                force,
            };
            let mut stdin = std::io::stdin();
            let stdout = std::io::stdout();
            let stderr = std::io::stderr();
            let mut stdout_lock = stdout.lock();
            let mut stderr_lock = stderr.lock();
            hooks::install_redirect_hook(
                &options,
                &home,
                &cwd,
                &mut stdin,
                &mut stdout_lock,
                &mut stderr_lock,
            )
        }
        HooksCommands::Uninstall { scope } => {
            let scope = hooks::Scope::parse(&scope)?;
            hooks::uninstall_redirect_hook(scope, &home, &cwd)?;
            Ok(0)
        }
        HooksCommands::Status => {
            let cwd_is_repo = cwd.join(".git").exists();
            let report = hooks::status_report(&home, &cwd, cwd_is_repo)?;
            for line in &report.lines {
                println!("{line}");
            }
            Ok(0)
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve => {
            server::run_server().await?;
        }
        Commands::Manifest {
            range,
            repo,
            page_size,
            include_function_analysis,
            max_response_tokens,
        } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let options = ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis,
                max_response_tokens: if max_response_tokens == 0 {
                    None
                } else {
                    Some(max_response_tokens)
                },
            };
            let manifest = match parse_range(&range) {
                RefRange::CommitRange { base, head } => {
                    collect_all_manifest_pages(&repo_path, base, head, &options, page_size)?
                }
                RefRange::WorktreeCompare { base } => {
                    collect_all_worktree_manifest_pages(&repo_path, base, &options, page_size)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Commands::History {
            range,
            repo,
            page_size,
        } => {
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
                max_response_tokens: None,
            };
            let history =
                collect_all_history_pages(&repo_path, base_ref, head_ref, &options, page_size)?;
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
        Commands::Context {
            range,
            repo,
            cursor,
            page_size,
            function_names,
            max_response_tokens,
        } => {
            let repo_path = repo.map(PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().expect("cannot determine current directory")
            });
            let ref_range = parse_range(&range);
            validate_commit_range(&ref_range, "context")?;
            let (base_ref, head_ref) = match ref_range {
                RefRange::CommitRange { base, head } => (base, head),
                RefRange::WorktreeCompare { .. } => unreachable!("validated above"),
            };
            let options = ContextOptions {
                cursor,
                page_size,
                function_names: function_names
                    .map(|s| s.split(',').map(|n| n.trim().to_string()).collect()),
                max_response_tokens: if max_response_tokens == 0 {
                    None
                } else {
                    Some(max_response_tokens)
                },
            };
            let context: FunctionContextResponse =
                build_function_context_with_options(&repo_path, base_ref, head_ref, &options)?;
            println!("{}", serde_json::to_string_pretty(&context)?);
        }
        Commands::Hooks { command } => {
            let exit_code = run_hooks_command(command)?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Commands::Languages => {
            println!("Supported languages for function-level analysis:");
            println!("  c          (.c, .h)");
            println!("  cpp        (.cpp, .hpp, .cc, .cxx, .hh, .hxx)");
            println!("  csharp     (.cs)");
            println!("  go         (.go)");
            println!("  java       (.java)");
            println!("  javascript (.js, .jsx)");
            println!("  kotlin     (.kt, .kts)");
            println!("  php        (.php)");
            println!("  python     (.py)");
            println!("  ruby       (.rb)");
            println!("  rust       (.rs)");
            println!("  swift      (.swift)");
            println!("  typescript (.ts, .tsx)");
        }
    }

    Ok(())
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
