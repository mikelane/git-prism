mod agent_detection;
mod git;
mod hooks;
pub(crate) mod metrics;
pub(crate) mod pagination;
pub(crate) mod privacy;
mod server;
// The shim module is complete but not yet wired to argv[0] dispatch (#287).
#[allow(dead_code)]
mod shim;
mod shim_cmd;
mod telemetry;
mod tools;
mod treesitter;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use git::refs::{RefRange, parse_range, validate_commit_range};
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
        /// Include diff hunk boundaries for modified files
        #[arg(long)]
        include_diff_hunks: bool,
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
    /// Detect whether the current process is running on behalf of an AI agent
    AgentDetect,
    /// Install, uninstall, or query the PATH shim (creates ~/.local/share/git-prism/bin/git)
    Shim {
        #[command(subcommand)]
        command: ShimCommands,
    },
    /// Uninstall legacy redirect-hook entries; query/install/uninstall the PATH shim
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install the PATH shim (redirect hook removed in v0.9.0; use --path-shim or `git-prism shim install`)
    Install {
        /// Accepted for backwards compatibility; ignored (redirect hook removed in v0.9.0)
        #[arg(long, value_parser = ["user", "project", "local"])]
        scope: Option<String>,
        /// Accepted for backwards compatibility; ignored
        #[arg(long)]
        dry_run: bool,
        /// Overwrite a pre-existing regular file at the path-shim target (use with caution)
        #[arg(long)]
        force: bool,
        /// Deprecated alias for `git-prism shim install`; still works with a warning
        #[arg(long)]
        path_shim: bool,
    },
    /// Remove redirect-hook entries written by this binary
    Uninstall {
        /// Which scope to clean up (required unless --path-shim is set)
        #[arg(long, value_parser = ["user", "project", "local"])]
        scope: Option<String>,
        /// Remove the PATH shim symlink (~/.local/share/git-prism/bin/git)
        #[arg(long)]
        path_shim: bool,
    },
    /// Report which scopes have the redirect hook installed
    Status,
}

#[derive(Subcommand)]
enum ShimCommands {
    /// Create ~/.local/share/git-prism/bin/git symlink pointing at this binary
    Install {
        /// Overwrite a regular file at the shim target (use with caution)
        #[arg(long)]
        force: bool,
    },
    /// Remove the ~/.local/share/git-prism/bin/git symlink
    Uninstall,
    /// Report whether the shim is installed and show the shim directory
    Status,
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
            dry_run: _,
            force,
            path_shim,
        } => {
            if path_shim {
                // Deprecated alias — still works but warns.
                eprintln!(
                    "warning: --path-shim is deprecated; use `git-prism shim install` instead"
                );
                let symlink_path = hooks::install_path_shim(&home, force)?;
                println!("Created symlink: {}", symlink_path.display());
                println!(
                    "Add this to your shell init (~/.zshrc or ~/.bashrc):\n  export PATH=\"$HOME/.local/share/git-prism/bin:$PATH\""
                );
                return Ok(0);
            }
            // hooks install without --path-shim: the redirect hook was removed in v0.9.0.
            // Scope argument is accepted by clap but ignored — we always error.
            let _ = scope;
            eprintln!(
                "error: the redirect hook (bash_redirect_hook.py) was removed in v0.9.0.\n\
                 \n\
                 Use the PATH shim instead:\n\
                 \n\
                 \x20 git-prism shim install\n\
                 \n\
                 The shim intercepts git at the PATH layer and is a strict superset of the\n\
                 redirect hook's coverage. If you had the old hook installed, remove it first:\n\
                 \n\
                 \x20 git-prism hooks uninstall --scope user   # or --scope project / local\n\
                 \n\
                 See docs/decisions/0011-redirect-hook-removal.md for details."
            );
            Ok(1)
        }
        HooksCommands::Uninstall { scope, path_shim } => {
            if scope.is_none() && !path_shim {
                anyhow::bail!("--scope is required unless --path-shim is set");
            }
            if let Some(scope_str) = scope {
                let scope = hooks::Scope::parse(&scope_str)?;
                hooks::uninstall_redirect_hook(scope, &home, &cwd)?;
            }
            if path_shim {
                hooks::uninstall_path_shim(&home)?;
            }
            Ok(0)
        }
        HooksCommands::Status => {
            let cwd_is_repo = cwd.join(".git").exists();
            let report = hooks::status_report(&home, &cwd, cwd_is_repo)?;
            for line in &report.lines {
                println!("{line}");
            }
            let shim_status = hooks::path_shim_status(&home);
            match shim_status {
                hooks::PathShimStatus::Installed { target } => {
                    println!("path-shim: installed @ {}", target.display());
                }
                hooks::PathShimStatus::NotInstalled => {
                    println!("path-shim: not installed");
                }
                hooks::PathShimStatus::BrokenLink { reason } => {
                    println!("path-shim: broken link ({reason})");
                }
            }
            Ok(0)
        }
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // argv[0] dispatch: when invoked as "git" (via a symlink), enter shim mode.
    let args_os: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let basename = args_os
        .first()
        .and_then(|s| std::path::Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    // Enter shim mode when invoked as "git" or "gh" (via a symlink).
    if basename == "git" || basename == "gh" {
        let args: Vec<&str> = args_os.iter().filter_map(|s| s.to_str()).collect();
        let exec = shim::real_git::StdRealGitExec {
            env: &agent_detection::StdEnvSource,
            argv0: args.first().copied().unwrap_or("git"),
        };
        // run_shim either exec-replaces the process (passthrough, never returns)
        // or returns ExitCode after printing structured JSON or an error.
        return shim::run_shim(&args, &agent_detection::StdEnvSource, &exec);
    }

    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> anyhow::Result<()> {
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
        Commands::Snapshot {
            range,
            paths,
            repo,
            include_diff_hunks,
        } => {
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
                include_diff_hunks,
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
        Commands::AgentDetect => {
            let result = agent_detection::detect_calling_agent(&agent_detection::StdEnvSource);
            #[derive(serde::Serialize)]
            struct Output {
                agent: Option<agent_detection::AgentName>,
                signal: Option<agent_detection::DetectionSignal>,
            }
            let output = match result {
                Some(detected) => Output {
                    agent: Some(detected.name),
                    signal: Some(detected.signal),
                },
                None => Output {
                    agent: None,
                    signal: None,
                },
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        Commands::Shim { command } => {
            let home = hooks::home_dir()?;
            match command {
                ShimCommands::Install { force } => shim_cmd::run_install(&home, force)?,
                ShimCommands::Uninstall => shim_cmd::run_uninstall(&home)?,
                ShimCommands::Status => shim_cmd::run_status(&home)?,
            }
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
