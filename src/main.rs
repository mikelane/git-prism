mod agent_detection;
mod git;
mod hooks;
pub(crate) mod metrics;
pub(crate) mod pagination;
pub(crate) mod privacy;
mod server;
mod shim;
mod shim_cmd;
mod telemetry;
mod tools;
mod treesitter;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};

use git::refs::{RefRange, parse_range, validate_commit_range};
use shim::real_git::RealGitExec as _;
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
    /// Install, uninstall, or query the interactive-shell `git` function (an rc-file block)
    Shim {
        #[command(subcommand)]
        command: ShimCommands,
    },
    /// Uninstall legacy redirect-hook entries; deprecated aliases for `shim install|uninstall|status`
    Hooks {
        #[command(subcommand)]
        command: HooksCommands,
    },
}

#[derive(Subcommand)]
enum HooksCommands {
    /// Install the shell-function shim (redirect hook removed in v0.9.0; use `git-prism shim install`)
    Install {
        /// Accepted for backwards compatibility; ignored (redirect hook removed in v0.9.0)
        #[arg(long, value_parser = ["user", "project", "local"])]
        scope: Option<String>,
        /// Accepted for backwards compatibility; ignored
        #[arg(long)]
        dry_run: bool,
        /// Accepted for backwards compatibility; ignored (no symlink is created any more)
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
        /// Remove the shim function block and legacy PATH-shim artifacts
        #[arg(long)]
        path_shim: bool,
    },
    /// Report which scopes have the redirect hook installed
    Status,
}

#[derive(Subcommand)]
enum ShimCommands {
    /// Add a `git` shell function to your interactive rc file (~/.zshrc or ~/.bashrc)
    Install {
        /// Accepted for backwards compatibility; ignored (no symlink is created any more)
        #[arg(long)]
        force: bool,
    },
    /// Remove the shell function block and any legacy PATH-shim symlinks/blocks
    Uninstall,
    /// Report whether the shell function is installed in the rc file and flag any legacy PATH shim
    Status,
}

fn resolve_cli_repo_path(
    repo: Option<String>,
    cwd: impl Fn() -> std::io::Result<PathBuf>,
) -> anyhow::Result<PathBuf> {
    match repo {
        Some(r) => Ok(PathBuf::from(r)),
        None => cwd().context("cannot determine current working directory"),
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
            path_shim,
        } => {
            if path_shim {
                // Deprecated alias — still works but warns.
                eprintln!(
                    "warning: --path-shim is deprecated; use `git-prism shim install` instead"
                );
                let _ = force;
                if dry_run {
                    println!(
                        "dry-run: would add the git shim function to your interactive rc file (not written)"
                    );
                    return Ok(0);
                }
                shim_cmd::run_install(&home)?;
                return Ok(0);
            }
            // hooks install without --path-shim: the redirect hook was removed in v0.9.0.
            // Scope argument is accepted by clap but ignored — we always error.
            let _ = scope;
            eprintln!(
                "error: the redirect hook (bash_redirect_hook.py) was removed in v0.9.0.\n\
                 \n\
                 Use the shell-function shim instead:\n\
                 \n\
                 \x20 git-prism shim install\n\
                 \n\
                 The shim installs a `git` shell function that routes agent-issued git commands\n\
                 through git-prism, and is a strict superset of the redirect hook's coverage. If you had the old hook installed, remove it first:\n\
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
                shim_cmd::run_uninstall(&home)?;
            }
            Ok(0)
        }
        HooksCommands::Status => {
            let cwd_is_repo = cwd.join(".git").exists();
            let report = hooks::status_report(&home, &cwd, cwd_is_repo)?;
            for line in &report.lines {
                println!("{line}");
            }
            shim_cmd::run_status(&home)?;
            Ok(0)
        }
    }
}

const SHIM_RUN_USAGE: &str = "usage: git-prism shim-run git <args...>";

/// Entry point for `git-prism shim-run git <args...>`, called by the rc shell
/// function. `shim_argv` is everything after `shim-run`.
fn run_shim_entry(shim_argv: &[&str]) -> std::process::ExitCode {
    if shim_argv.first() != Some(&"git") {
        eprintln!("{SHIM_RUN_USAGE}");
        return std::process::ExitCode::from(2);
    }
    let current_exe = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(err) => {
            eprintln!("git-prism shim-run: cannot locate own executable: {err}");
            return std::process::ExitCode::from(127);
        }
    };
    let exec = shim::real_git::StdRealGitExec {
        env: &agent_detection::StdEnvSource,
        argv0: &current_exe,
    };

    // Per-invocation opt-out: exec the real binary immediately with zero
    // overhead (GIT_PRISM_PASSTHROUGH=1 / GIT_PRISM_DISABLE=1).
    if shim::passthrough_opt_out_requested(&agent_detection::StdEnvSource) {
        return exec.passthrough(shim_argv);
    }

    // Use `init_quiet`: this runs once per intercepted git command, so a
    // per-invocation banner would flood the developer's shell. On passthrough
    // paths run_shim flushes before exec; on the structured path we flush here.
    let mut telemetry_guard = telemetry::init_quiet();
    let exit_code = shim::run_shim(
        shim_argv,
        &agent_detection::StdEnvSource,
        &exec,
        shim::detect_stdout_kind(),
        &mut telemetry_guard,
    );
    telemetry_guard.force_flush_bounded(shim::STRUCTURED_FLUSH_TIMEOUT);
    exit_code
}

/// `str`-based argument handling would silently drop arguments that are not
/// valid UTF-8, handing git a different command than the user typed. When the
/// shim would act as a pure passthrough anyway (`shim-run git ...` or a stale
/// `git`/`gh` symlink), exec the real binary with the raw arguments instead.
#[cfg(unix)]
fn passthrough_when_args_are_not_utf8(
    args_os: &[std::ffi::OsString],
    basename: &str,
) -> Option<std::process::ExitCode> {
    if args_os.iter().all(|arg| arg.to_str().is_some()) {
        return None;
    }
    let argv0 = args_os.first()?.to_string_lossy().into_owned();
    let (binary_name, raw_args) = if basename == "git" || basename == "gh" {
        (basename, &args_os[1..])
    } else if args_os.get(1).and_then(|a| a.to_str()) == Some("shim-run")
        && args_os.get(2).and_then(|a| a.to_str()) == Some("git")
    {
        ("git", &args_os[3..])
    } else {
        return None;
    };
    Some(shim::real_git::exec_real_binary_with_raw_args(
        binary_name,
        &argv0,
        &agent_detection::StdEnvSource,
        raw_args,
    ))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args_os: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let args: Vec<&str> = args_os.iter().filter_map(|s| s.to_str()).collect();
    let basename = args_os
        .first()
        .and_then(|s| std::path::Path::new(s).file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    #[cfg(unix)]
    if let Some(exit_code) = passthrough_when_args_are_not_utf8(&args_os, &basename) {
        return exit_code;
    }

    // A binary reached as "git" or "gh" (a stale PATH symlink from a pre-0.11
    // install) must be invisible: any program that execs git gets real git.
    // Interception is opt-in via `git-prism shim-run git ...` (see #387).
    if basename == "git" || basename == "gh" {
        let exec = shim::real_git::StdRealGitExec {
            env: &agent_detection::StdEnvSource,
            argv0: args.first().copied().unwrap_or("git"),
        };
        return exec.passthrough(&args);
    }

    if args.get(1) == Some(&"shim-run") {
        return run_shim_entry(&args[2..]);
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
            let repo_path = resolve_cli_repo_path(repo, std::env::current_dir)?;
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
            let repo_path = resolve_cli_repo_path(repo, std::env::current_dir)?;
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
            let repo_path = resolve_cli_repo_path(repo, std::env::current_dir)?;
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
            let repo_path = resolve_cli_repo_path(repo, std::env::current_dir)?;
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
                ShimCommands::Install { force: _ } => shim_cmd::run_install(&home)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_uses_explicit_repo_without_calling_cwd() {
        let result = resolve_cli_repo_path(Some("/some/path".into()), || unreachable!());
        assert_eq!(result.unwrap(), PathBuf::from("/some/path"));
    }

    #[test]
    fn it_falls_back_to_cwd_when_repo_is_none() {
        let result = resolve_cli_repo_path(None, || Ok(PathBuf::from("/cwd")));
        assert_eq!(result.unwrap(), PathBuf::from("/cwd"));
    }

    #[test]
    fn it_returns_clean_error_when_cwd_is_unavailable() {
        let result = resolve_cli_repo_path(None, || {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "boom"))
        });
        let err = result.unwrap_err();
        let formatted = format!("{err:#}");
        assert!(
            formatted.contains("cannot determine current working directory"),
            "expected error context in: {formatted}"
        );
    }

    #[test]
    fn it_treats_empty_repo_string_as_an_explicit_path() {
        // `--repo ""` parses to Some(String::new()); the helper returns it as an
        // explicit empty path (gix resolves "" to cwd downstream) rather than
        // falling back via cwd. Pin this so a future is_empty() fallback is a
        // deliberate choice, not an accident.
        let result = resolve_cli_repo_path(Some(String::new()), || unreachable!());
        assert_eq!(result.unwrap(), std::path::PathBuf::from(""));
    }
}
