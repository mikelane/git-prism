mod git;
pub(crate) mod metrics;
#[allow(dead_code)]
pub(crate) mod pagination;
#[allow(dead_code)]
pub(crate) mod privacy;
mod server;
mod telemetry;
mod tools;
mod treesitter;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use pagination::decode_cursor;
use tools::{
    HistoryResponse, ManifestOptions, ManifestResponse, SnapshotOptions, build_history,
    build_manifest, build_snapshots, build_worktree_manifest,
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
    let page_size = crate::pagination::clamp_page_size(page_size);
    let mut all_files = Vec::new();

    let first_page = build_manifest(repo_path, base, head, options, 0, page_size)?;
    all_files.extend(first_page.files);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = build_manifest(repo_path, base, head, options, cursor.offset, page_size)?;
        all_files.extend(page.files);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_files.len();
    Ok(ManifestResponse {
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
    })
}

/// Collect all manifest pages in worktree mode into a single combined response.
fn collect_all_worktree_manifest_pages(
    repo_path: &Path,
    base: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    let page_size = crate::pagination::clamp_page_size(page_size);
    let mut all_files = Vec::new();

    let first_page = build_worktree_manifest(repo_path, base, options, 0, page_size)?;
    all_files.extend(first_page.files);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = build_worktree_manifest(repo_path, base, options, cursor.offset, page_size)?;
        all_files.extend(page.files);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_files.len();
    Ok(ManifestResponse {
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
    })
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
        };

        let result = collect_all_history_pages(&path, "HEAD~3", "HEAD", &options, 1).unwrap();

        assert_eq!(result.commits.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
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
        } => {
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
        Commands::Languages => {
            println!("Supported languages for function-level analysis:");
            println!("  c          (.c, .h)");
            println!("  cpp        (.cpp, .hpp, .cc, .cxx, .hh, .hxx)");
            println!("  go         (.go)");
            println!("  java       (.java)");
            println!("  javascript (.js, .jsx)");
            println!("  kotlin     (.kt, .kts)");
            println!("  python     (.py)");
            println!("  rust       (.rs)");
            println!("  typescript (.ts, .tsx)");
        }
    }

    Ok(())
}
