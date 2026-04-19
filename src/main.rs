mod git;
pub(crate) mod metrics;
pub(crate) mod pagination;
pub(crate) mod privacy;
mod server;
mod telemetry;
mod tools;
mod treesitter;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use pagination::decode_cursor;
use tools::{
    ContextOptions, FunctionContextResponse, HistoryResponse, ManifestOptions, ManifestResponse,
    SnapshotOptions, build_function_context_with_options, build_history, build_manifest,
    build_snapshots, build_worktree_manifest, enforce_token_budget,
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

/// Assemble paginated manifest pages into a single combined `ManifestResponse`,
/// then apply token-budget enforcement on the combined result.
///
/// `fetch_page` is called with `(offset, page_size)` and must return one page.
/// The first call is always at offset 0; subsequent calls use the cursor offset
/// from the previous page's `pagination.next_cursor`.
///
/// Budget enforcement is intentionally deferred to the combined response:
/// per-page enforcement would cap each page individually but the caller would
/// re-assemble them all, defeating the budget.
fn collect_manifest_pages(
    fetch_page: impl Fn(usize, usize) -> anyhow::Result<ManifestResponse>,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    let page_size = crate::pagination::clamp_page_size(page_size);

    let mut all_files = Vec::new();

    let first_page = fetch_page(0, page_size)?;
    all_files.extend(first_page.files);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = fetch_page(cursor.offset, page_size)?;
        all_files.extend(page.files);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_files.len();
    let mut response = ManifestResponse {
        metadata: first_page.metadata,
        summary: first_page.summary,
        files: all_files,
        dependency_changes: first_page.dependency_changes,
        pagination: pagination::PaginationInfo {
            total_items,
            page_start: 0,
            page_size: total_items,
            next_cursor: None,
        },
    };

    // Apply budget enforcement on the combined response
    if let Some(budget) = options.max_response_tokens.filter(|&b| b > 0)
        && options.include_function_analysis
    {
        let trimmed = enforce_token_budget(&mut response, budget);
        response.metadata.function_analysis_truncated = trimmed;
        // Sync page_size to the actual returned file count so callers can detect
        // file dropping: response.files.len() < pagination.total_items means
        // some files were dropped by the token budget. next_cursor is not set
        // in combined mode; callers must re-request with a larger budget or
        // without enforcement (max_response_tokens: 0) to get all files.
        response.pagination.page_size = response.files.len();
    }
    response.metadata.token_estimate = tools::size::estimate_response_tokens(&response);

    Ok(response)
}

/// Collect all manifest pages into a single combined response.
///
/// Loops through pages using the given `page_size` until `next_cursor` is `None`,
/// accumulating all file entries. Returns a single `ManifestResponse` with the
/// first page's metadata/summary and all collected files.
fn collect_all_manifest_pages(
    repo_path: &Path,
    base: &str,
    head: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    // Disable per-page budget enforcement during collection — enforced once on
    // the combined response inside `collect_manifest_pages`.
    let collection_options = ManifestOptions {
        max_response_tokens: None,
        ..options.clone()
    };
    collect_manifest_pages(
        |offset, ps| {
            build_manifest(repo_path, base, head, &collection_options, offset, ps)
                .map_err(anyhow::Error::from)
        },
        options,
        page_size,
    )
}

/// Collect all manifest pages in worktree mode into a single combined response.
fn collect_all_worktree_manifest_pages(
    repo_path: &Path,
    base: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    // Disable per-page budget enforcement during collection — enforced once on
    // the combined response inside `collect_manifest_pages`.
    let collection_options = ManifestOptions {
        max_response_tokens: None,
        ..options.clone()
    };
    collect_manifest_pages(
        |offset, ps| {
            build_worktree_manifest(repo_path, base, &collection_options, offset, ps)
                .map_err(anyhow::Error::from)
        },
        options,
        page_size,
    )
}

/// Collect all history pages into a single combined response.
fn collect_all_history_pages(
    repo_path: &Path,
    base: &str,
    head: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<HistoryResponse> {
    let page_size = crate::pagination::clamp_page_size(page_size);
    let mut all_commits = Vec::new();

    let first_page = build_history(repo_path, base, head, options, 0, page_size)?;
    all_commits.extend(first_page.commits);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = build_history(repo_path, base, head, options, cursor.offset, page_size)?;
        all_commits.extend(page.commits);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_commits.len();
    Ok(HistoryResponse {
        commits: all_commits,
        pagination: pagination::PaginationInfo {
            total_items,
            page_start: 0,
            page_size: total_items,
            next_cursor: None,
        },
    })
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

    // --- Auto-pagination helper tests ---

    use std::process::Command;
    use tempfile::TempDir;

    /// Create a repo with many file changes so pagination kicks in with small page sizes.
    fn create_repo_with_many_files(file_count: usize) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Base commit
        std::fs::write(path.join("README.md"), "# base\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "base commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Add many files in a second commit
        for i in 0..file_count {
            std::fs::write(path.join(format!("file_{i}.txt")), format!("content {i}\n")).unwrap();
        }
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add many files"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    fn create_repo_with_many_commits(commit_count: usize) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Anchor commit
        std::fs::write(path.join("README.md"), "# anchor\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "anchor"])
            .current_dir(&path)
            .output()
            .unwrap();

        for i in 0..commit_count {
            std::fs::write(path.join(format!("file_{i}.txt")), format!("content {i}\n")).unwrap();
            Command::new("git")
                .args(["add", "."])
                .current_dir(&path)
                .output()
                .unwrap();
            Command::new("git")
                .args(["commit", "-m", &format!("commit {i}")])
                .current_dir(&path)
                .output()
                .unwrap();
        }

        (dir, path)
    }

    #[test]
    fn collect_all_manifest_pages_returns_all_files_when_single_page() {
        let (_dir, path) = create_repo_with_many_files(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 500).unwrap();

        assert_eq!(result.files.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 3);
    }

    #[test]
    fn collect_all_manifest_pages_collects_across_multiple_pages() {
        let (_dir, path) = create_repo_with_many_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // Use page_size=2, so we need 3 pages for 5 files
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 2).unwrap();

        assert_eq!(result.files.len(), 5);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 5);
    }

    #[test]
    fn collect_all_manifest_pages_preserves_metadata_from_first_page() {
        let (_dir, path) = create_repo_with_many_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 2).unwrap();

        assert_eq!(result.metadata.base_ref, "HEAD~1");
        assert_eq!(result.metadata.head_ref, "HEAD");
        assert!(!result.metadata.base_sha.is_empty());
        assert!(!result.metadata.head_sha.is_empty());
    }

    #[test]
    fn collect_all_manifest_pages_with_page_size_1() {
        let (_dir, path) = create_repo_with_many_files(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // page_size=1 forces 3 pages
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 1).unwrap();

        assert_eq!(result.files.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
    }

    #[test]
    fn collect_all_history_pages_returns_all_commits_when_single_page() {
        let (_dir, path) = create_repo_with_many_commits(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = collect_all_history_pages(&path, "HEAD~3", "HEAD", &options, 500).unwrap();

        assert_eq!(result.commits.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 3);
    }

    #[test]
    fn collect_all_history_pages_collects_across_multiple_pages() {
        let (_dir, path) = create_repo_with_many_commits(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // page_size=2 forces 3 pages for 5 commits
        let result = collect_all_history_pages(&path, "HEAD~5", "HEAD", &options, 2).unwrap();

        assert_eq!(result.commits.len(), 5);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 5);
    }

    #[test]
    fn collect_all_history_pages_preserves_commit_order() {
        let (_dir, path) = create_repo_with_many_commits(4);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = collect_all_history_pages(&path, "HEAD~4", "HEAD", &options, 2).unwrap();

        assert_eq!(result.commits.len(), 4);
        assert_eq!(result.commits[0].metadata.message, "commit 0");
        assert_eq!(result.commits[1].metadata.message, "commit 1");
        assert_eq!(result.commits[2].metadata.message, "commit 2");
        assert_eq!(result.commits[3].metadata.message, "commit 3");
    }

    #[test]
    fn collect_all_history_pages_with_page_size_1() {
        let (_dir, path) = create_repo_with_many_commits(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = collect_all_history_pages(&path, "HEAD~3", "HEAD", &options, 1).unwrap();

        assert_eq!(result.commits.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
    }
}
