pub mod context;
pub mod history;
pub mod import_scope;
pub mod manifest;
pub mod review_change;
pub mod size;
pub mod snapshots;
pub mod types;

pub use context::{ContextOptions, build_function_context_with_options};
pub use history::build_history;
pub use manifest::{build_manifest, build_worktree_manifest, enforce_token_budget};
pub use review_change::{ReviewChangeArgs, ReviewChangeResponse, build_review_change};
pub use snapshots::build_snapshots;
pub use types::{
    ContextArgs, FunctionContextResponse, HistoryArgs, HistoryResponse, ManifestArgs,
    ManifestOptions, ManifestResponse, SnapshotArgs, SnapshotOptions, SnapshotResponse,
};

/// Extract the file extension from a path.
///
/// Returns the substring after the final `.`, or `""` when the path has no
/// extension or the "extension" is actually a dotfile basename like
/// `.gitignore`. The `path.len() > ext.len() + 1` guard rejects the
/// dotfile case: `.gitignore` splits as `("", "gitignore")` but the full
/// path length equals the candidate extension length plus one character
/// for the leading dot, which means there's no real basename before it.
///
/// Used by the manifest and context tools to look up language analyzers.
/// `pub(crate)` rather than `pub` so the helper stays an internal
/// orchestration detail — downstream MCP clients should not depend on it.
pub(crate) fn extension_from_path(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .filter(|ext| path.len() > ext.len() + 1)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_from_path_returns_extension_for_simple_file() {
        assert_eq!(extension_from_path("lib.rs"), "rs");
    }

    #[test]
    fn extension_from_path_returns_last_extension_for_double_extension() {
        assert_eq!(extension_from_path("foo.test.ts"), "ts");
    }

    #[test]
    fn extension_from_path_returns_empty_for_no_extension() {
        assert_eq!(extension_from_path("Makefile"), "");
    }

    #[test]
    fn extension_from_path_returns_empty_for_empty_string() {
        assert_eq!(extension_from_path(""), "");
    }

    #[test]
    fn extension_from_path_handles_nested_path() {
        assert_eq!(extension_from_path("src/tools/context.rs"), "rs");
    }

    #[test]
    fn extension_from_path_returns_empty_for_dotfile() {
        // `.gitignore` splits as ("", "gitignore") but path.len() == ext.len() + 1
        // so the filter rejects it and returns "".
        assert_eq!(extension_from_path(".gitignore"), "");
    }
}
