use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::git::depfiles::DependencyDiff;
use crate::git::diff::{ChangeScope, ChangeType};
use crate::pagination::PaginationInfo;

// --- FunctionChange ---

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FunctionChangeType {
    Added,
    Modified,
    Deleted,
    SignatureChanged,
    Renamed,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FunctionChange {
    pub name: String,
    /// For renames, the original function name. Null otherwise.
    pub old_name: Option<String>,
    pub change_type: FunctionChangeType,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: String,
}

impl FunctionChange {
    /// Build a change entry from a [`Function`] reference.
    pub fn from_function(
        f: &crate::treesitter::Function,
        change_type: FunctionChangeType,
        old_name: Option<String>,
    ) -> Self {
        Self {
            name: f.name.clone(),
            old_name,
            change_type,
            start_line: f.start_line,
            end_line: f.end_line,
            signature: f.signature.clone(),
        }
    }
}

// --- ImportChange ---

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImportChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

// --- Manifest types ---

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ManifestMetadata {
    pub repo_path: String,
    pub base_ref: String,
    pub head_ref: String,
    pub base_sha: String,
    pub head_sha: String,
    pub generated_at: DateTime<Utc>,
    pub version: String,
    /// Estimated token count of the serialized response, via the ~4-chars-
    /// per-token heuristic in [`crate::tools::size::estimate_response_tokens`].
    /// Agents use this as a cheap pre-flight hint before requesting a follow-
    /// up call (e.g., `get_function_context` on the same range). Populated in
    /// a two-pass build: the response is first constructed with `0`, then the
    /// estimate is computed on that struct and written back. The final value
    /// is therefore a lower bound that undercounts by the single-digit
    /// character delta between `"token_estimate":0` and the real value, which
    /// is acceptable for a budgeting hint.
    pub token_estimate: usize,
    /// Paths of files whose function analysis was trimmed to fit the token
    /// budget. Only includes "tier 1" files (signatures preserved, imports
    /// stripped). Files stripped to bare entries are not listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub function_analysis_truncated: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ManifestSummary {
    pub total_files_changed: usize,
    pub files_added: usize,
    pub files_modified: usize,
    pub files_deleted: usize,
    pub files_renamed: usize,
    pub total_lines_added: usize,
    pub total_lines_removed: usize,
    pub total_functions_changed: Option<usize>,
    pub languages_affected: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ManifestFileEntry {
    pub path: String,
    pub old_path: Option<String>,
    pub change_type: ChangeType,
    pub change_scope: ChangeScope,
    pub language: String,
    pub is_binary: bool,
    pub is_generated: bool,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub size_before: usize,
    pub size_after: usize,
    pub functions_changed: Option<Vec<FunctionChange>>,
    pub imports_changed: Option<ImportChange>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ManifestResponse {
    pub metadata: ManifestMetadata,
    pub summary: ManifestSummary,
    pub files: Vec<ManifestFileEntry>,
    pub dependency_changes: Vec<DependencyDiff>,
    pub pagination: PaginationInfo,
}

// --- Snapshot types ---

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotMetadata {
    pub repo_path: String,
    pub base_ref: String,
    pub head_ref: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FileContent {
    pub content: String,
    pub line_count: usize,
    pub size_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotFileEntry {
    pub path: String,
    pub language: String,
    pub is_binary: bool,
    pub before: Option<FileContent>,
    pub after: Option<FileContent>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotResponse {
    pub metadata: SnapshotMetadata,
    pub files: Vec<SnapshotFileEntry>,
    pub token_estimate: usize,
}

// --- MCP tool input types ---

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ManifestArgs {
    pub base_ref: String,
    pub head_ref: Option<String>,
    pub repo_path: Option<String>,
    #[serde(default)]
    pub include_patterns: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub include_function_analysis: bool,
    /// Maximum estimated tokens for the full response. When exceeded,
    /// function/import analysis is progressively stripped per file.
    /// Default 8192. Pass 0 to disable budget enforcement.
    #[serde(default = "default_token_budget")]
    pub max_response_tokens: usize,
    /// Opaque pagination cursor from a previous response.
    pub cursor: Option<String>,
    /// Maximum file entries per page (1-500, default 100).
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SnapshotArgs {
    pub base_ref: String,
    pub head_ref: Option<String>,
    pub paths: Vec<String>,
    pub repo_path: Option<String>,
    #[serde(default = "default_true")]
    pub include_before: bool,
    #[serde(default = "default_true")]
    pub include_after: bool,
    #[serde(default = "default_max_file_size")]
    pub max_file_size_bytes: usize,
    pub line_range: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct HistoryArgs {
    pub base_ref: String,
    pub head_ref: String,
    pub repo_path: Option<String>,
    /// Opaque pagination cursor from a previous response.
    pub cursor: Option<String>,
    /// Maximum commits per page (1-500, default 100).
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContextArgs {
    /// Base git ref (exclusive). Commits after this are considered.
    pub base_ref: String,
    /// Head git ref (inclusive). Required — this tool does not support working
    /// tree mode because callers/callees are resolved from committed content.
    pub head_ref: String,
    /// Path to the git repository (defaults to the server's working directory).
    pub repo_path: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_token_budget() -> usize {
    8192
}

fn default_max_file_size() -> usize {
    100_000
}

fn default_page_size() -> usize {
    100
}

// --- History types ---

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommitMetadata {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CommitManifest {
    pub metadata: CommitMetadata,
    pub files: Vec<ManifestFileEntry>,
    pub summary: ManifestSummary,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HistoryResponse {
    pub commits: Vec<CommitManifest>,
    pub pagination: PaginationInfo,
}

// --- FunctionContext types ---

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CallerEntry {
    pub file: String,
    pub line: usize,
    pub caller: String,
    pub is_test: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CalleeEntry {
    pub callee: String,
    pub line: usize,
}

/// How the caller scan was performed for a given function.
///
/// `Scoped` means the scan used import-based filtering and may have excluded
/// files that don't explicitly import the changed module. `Fallback` means the
/// scan parsed every file in the repo (current behavior for languages without
/// import-scoping support). Agents can use this to tell whether a zero-caller
/// result is authoritative or potentially incomplete.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopingMode {
    Scoped,
    Fallback,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FunctionContextEntry {
    pub name: String,
    pub file: String,
    pub change_type: FunctionChangeType,
    pub blast_radius: BlastRadius,
    pub scoping_mode: ScopingMode,
    pub callers: Vec<CallerEntry>,
    pub callees: Vec<CalleeEntry>,
    pub test_references: Vec<CallerEntry>,
    pub caller_count: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BlastRadius {
    pub production_callers: usize,
    pub test_callers: usize,
    pub has_tests: bool,
    pub risk: RiskLevel,
}

impl BlastRadius {
    #[must_use]
    pub fn compute(production_callers: usize, test_callers: usize) -> Self {
        let has_tests = test_callers > 0;
        let risk = match (production_callers, has_tests) {
            (0, _) => RiskLevel::None,
            (1..=2, true) => RiskLevel::Low,
            (1..=2, false) => RiskLevel::Medium,
            (_, true) => RiskLevel::Medium,
            (_, false) => RiskLevel::High,
        };
        Self {
            production_callers,
            test_callers,
            has_tests,
            risk,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ContextMetadata {
    pub base_ref: String,
    pub head_ref: String,
    pub base_sha: String,
    pub head_sha: String,
    pub generated_at: DateTime<Utc>,
    /// Estimated token count of the serialized response. See
    /// [`ManifestMetadata::token_estimate`] for the semantics and caveats;
    /// the same two-pass construction trick applies.
    pub token_estimate: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FunctionContextResponse {
    pub metadata: ContextMetadata,
    pub functions: Vec<FunctionContextEntry>,
}

// --- Tool options (for internal use) ---

#[derive(Debug, Clone)]
pub struct ManifestOptions {
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub include_function_analysis: bool,
    /// Token budget for the response. `None` disables enforcement (used by
    /// internal callers like `context.rs` that need full data). `Some(n)`
    /// triggers per-file tiered trimming when the response exceeds `n` tokens.
    pub max_response_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SnapshotOptions {
    pub include_before: bool,
    pub include_after: bool,
    pub max_file_size_bytes: usize,
    pub line_range: Option<(usize, usize)>,
}

// --- ToolError ---

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("git error: {0}")]
    Git(#[from] crate::git::reader::GitError),
}

// --- Helper ---

pub fn detect_language(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "go" => "go",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "rb" => "ruby",
        "rs" => "rust",
        "java" => "java",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_detects_go_language_from_extension() {
        assert_eq!(detect_language("main.go"), "go");
    }

    #[test]
    fn it_detects_python_language_from_extension() {
        assert_eq!(detect_language("app.py"), "python");
    }

    #[test]
    fn it_detects_typescript_from_ts_extension() {
        assert_eq!(detect_language("index.ts"), "typescript");
    }

    #[test]
    fn it_detects_typescript_from_tsx_extension() {
        assert_eq!(detect_language("App.tsx"), "typescript");
    }

    #[test]
    fn it_detects_javascript_from_js_extension() {
        assert_eq!(detect_language("util.js"), "javascript");
    }

    #[test]
    fn it_detects_javascript_from_jsx_extension() {
        assert_eq!(detect_language("Component.jsx"), "javascript");
    }

    #[test]
    fn it_detects_rust_from_rs_extension() {
        assert_eq!(detect_language("lib.rs"), "rust");
    }

    #[test]
    fn it_detects_java_from_java_extension() {
        assert_eq!(detect_language("Main.java"), "java");
    }

    #[test]
    fn it_detects_java_from_nested_path() {
        assert_eq!(detect_language("src/com/example/Main.java"), "java");
    }

    #[test]
    fn it_detects_swift_from_swift_extension() {
        assert_eq!(detect_language("App.swift"), "swift");
    }

    #[test]
    fn it_detects_c_from_c_extension() {
        assert_eq!(detect_language("main.c"), "c");
    }

    #[test]
    fn it_detects_c_from_h_extension() {
        assert_eq!(detect_language("utils.h"), "c");
    }

    #[test]
    fn it_detects_cpp_from_cpp_extension() {
        assert_eq!(detect_language("widget.cpp"), "cpp");
    }

    #[test]
    fn it_detects_cpp_from_hpp_extension() {
        assert_eq!(detect_language("widget.hpp"), "cpp");
    }

    #[test]
    fn it_detects_cpp_from_cc_extension() {
        assert_eq!(detect_language("widget.cc"), "cpp");
    }

    #[test]
    fn it_detects_cpp_from_cxx_extension() {
        assert_eq!(detect_language("widget.cxx"), "cpp");
    }

    #[test]
    fn it_detects_cpp_from_hh_extension() {
        assert_eq!(detect_language("widget.hh"), "cpp");
    }

    #[test]
    fn it_detects_cpp_from_hxx_extension() {
        assert_eq!(detect_language("widget.hxx"), "cpp");
    }

    #[test]
    fn it_detects_kotlin_from_kt_extension() {
        assert_eq!(detect_language("Main.kt"), "kotlin");
    }

    #[test]
    fn it_detects_kotlin_from_kts_extension() {
        assert_eq!(detect_language("build.gradle.kts"), "kotlin");
    }

    #[test]
    fn it_returns_unknown_for_unsupported_extension() {
        assert_eq!(detect_language("README.md"), "unknown");
    }

    #[test]
    fn it_returns_unknown_for_no_extension() {
        assert_eq!(detect_language("Makefile"), "unknown");
    }

    #[test]
    fn it_handles_nested_path_with_dots() {
        assert_eq!(detect_language("src/utils/helper.test.ts"), "typescript");
    }

    #[test]
    fn it_returns_unknown_for_empty_string() {
        assert_eq!(detect_language(""), "unknown");
    }

    #[test]
    fn manifest_response_serializes_to_json() {
        let response = ManifestResponse {
            metadata: ManifestMetadata {
                repo_path: "/tmp/repo".into(),
                base_ref: "HEAD~1".into(),
                head_ref: "HEAD".into(),
                base_sha: "abc123".into(),
                head_sha: "def456".into(),
                generated_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                version: "0.1.0".into(),
                token_estimate: 0,
                function_analysis_truncated: vec![],
            },
            summary: ManifestSummary {
                total_files_changed: 1,
                files_added: 1,
                files_modified: 0,
                files_deleted: 0,
                files_renamed: 0,
                total_lines_added: 5,
                total_lines_removed: 0,
                total_functions_changed: None,
                languages_affected: vec!["rust".into()],
            },
            files: vec![],
            dependency_changes: vec![],
            pagination: PaginationInfo {
                total_items: 1,
                page_start: 0,
                page_size: 200,
                next_cursor: None,
            },
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["metadata"]["base_ref"], "HEAD~1");
        assert_eq!(json["summary"]["total_files_changed"], 1);
        assert!(json["summary"]["total_functions_changed"].is_null());
        assert!(json["pagination"]["next_cursor"].is_null());
        assert_eq!(json["metadata"]["token_estimate"], 0);
    }

    #[test]
    fn snapshot_response_serializes_to_json() {
        let response = SnapshotResponse {
            metadata: SnapshotMetadata {
                repo_path: "/tmp/repo".into(),
                base_ref: "HEAD~1".into(),
                head_ref: "HEAD".into(),
                generated_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            files: vec![SnapshotFileEntry {
                path: "src/main.rs".into(),
                language: "rust".into(),
                is_binary: false,
                before: None,
                after: Some(FileContent {
                    content: "fn main() {}".into(),
                    line_count: 1,
                    size_bytes: 12,
                    truncated: false,
                }),
                error: None,
            }],
            token_estimate: 3,
        };
        let json = serde_json::to_value(&response).unwrap();
        assert!(json["files"][0]["before"].is_null());
        assert_eq!(json["files"][0]["after"]["line_count"], 1);
        assert_eq!(json["token_estimate"], 3);
    }

    #[test]
    fn function_change_type_serializes_as_snake_case() {
        let change = FunctionChange {
            name: "foo".into(),
            old_name: None,
            change_type: FunctionChangeType::SignatureChanged,
            start_line: 1,
            end_line: 5,
            signature: "fn foo(x: i32)".into(),
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["change_type"], "signature_changed");
        assert!(json["old_name"].is_null());
    }

    #[test]
    fn renamed_change_type_serializes_with_old_name() {
        let change = FunctionChange {
            name: "new_fn".into(),
            old_name: Some("old_fn".into()),
            change_type: FunctionChangeType::Renamed,
            start_line: 1,
            end_line: 5,
            signature: "fn new_fn()".into(),
        };
        let json = serde_json::to_value(&change).unwrap();
        assert_eq!(json["change_type"], "renamed");
        assert_eq!(json["old_name"], "old_fn");
        assert_eq!(json["name"], "new_fn");
    }

    #[test]
    fn manifest_args_deserializes_with_defaults() {
        let json = r#"{"base_ref": "main"}"#;
        let args: ManifestArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.base_ref, "main");
        assert!(args.head_ref.is_none());
        assert!(!args.include_function_analysis);
        assert_eq!(args.max_response_tokens, 8192);
        assert!(args.include_patterns.is_empty());
    }

    #[test]
    fn history_response_serializes_commits_array() {
        let response = HistoryResponse {
            commits: vec![CommitManifest {
                metadata: CommitMetadata {
                    sha: "abc123".into(),
                    message: "test commit".into(),
                    author: "Test User".into(),
                    timestamp: "2026-01-01T00:00:00Z".into(),
                },
                files: vec![],
                summary: ManifestSummary {
                    total_files_changed: 0,
                    files_added: 0,
                    files_modified: 0,
                    files_deleted: 0,
                    files_renamed: 0,
                    total_lines_added: 0,
                    total_lines_removed: 0,
                    total_functions_changed: None,
                    languages_affected: vec![],
                },
            }],
            pagination: PaginationInfo {
                total_items: 1,
                page_start: 0,
                page_size: 100,
                next_cursor: None,
            },
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["commits"].as_array().unwrap().len(), 1);
        assert_eq!(json["commits"][0]["metadata"]["sha"], "abc123");
        assert_eq!(json["commits"][0]["metadata"]["author"], "Test User");
        assert_eq!(
            json["commits"][0]["metadata"]["timestamp"],
            "2026-01-01T00:00:00Z"
        );
        assert_eq!(json["commits"][0]["metadata"]["message"], "test commit");
        assert!(json["commits"][0]["files"].as_array().unwrap().is_empty());
    }

    #[test]
    fn snapshot_args_deserializes_with_defaults() {
        let json = r#"{"base_ref": "main", "paths": ["src/main.rs"]}"#;
        let args: SnapshotArgs = serde_json::from_str(json).unwrap();
        assert!(args.include_before);
        assert!(args.include_after);
        assert_eq!(args.max_file_size_bytes, 100_000);
        assert!(args.line_range.is_none());
    }

    #[test]
    fn manifest_args_accepts_pagination_params() {
        let json =
            r#"{"base_ref": "main", "head_ref": "HEAD", "cursor": "abc123", "page_size": 50}"#;
        let args: ManifestArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.cursor.as_deref(), Some("abc123"));
        assert_eq!(args.page_size, 50);
    }

    #[test]
    fn manifest_args_defaults_pagination_when_omitted() {
        let json = r#"{"base_ref": "main"}"#;
        let args: ManifestArgs = serde_json::from_str(json).unwrap();
        assert!(args.cursor.is_none());
        assert_eq!(args.page_size, 100);
    }

    #[test]
    fn history_args_accepts_pagination_params() {
        let json =
            r#"{"base_ref": "HEAD~5", "head_ref": "HEAD", "cursor": "xyz", "page_size": 25}"#;
        let args: HistoryArgs = serde_json::from_str(json).unwrap();
        assert_eq!(args.cursor.as_deref(), Some("xyz"));
        assert_eq!(args.page_size, 25);
    }

    #[test]
    fn history_args_defaults_pagination_when_omitted() {
        let json = r#"{"base_ref": "HEAD~5", "head_ref": "HEAD"}"#;
        let args: HistoryArgs = serde_json::from_str(json).unwrap();
        assert!(args.cursor.is_none());
        assert_eq!(args.page_size, 100);
    }

    #[test]
    fn blast_radius_zero_callers_is_none_risk() {
        let br = BlastRadius::compute(0, 0);
        assert_eq!(br.risk, RiskLevel::None);
        assert!(!br.has_tests);
    }

    #[test]
    fn blast_radius_zero_production_with_tests_is_none_risk() {
        let br = BlastRadius::compute(0, 3);
        assert_eq!(br.risk, RiskLevel::None);
        assert!(br.has_tests);
    }

    #[test]
    fn blast_radius_low_callers_with_tests_is_low() {
        let br = BlastRadius::compute(2, 1);
        assert_eq!(br.risk, RiskLevel::Low);
        assert!(br.has_tests);
    }

    #[test]
    fn blast_radius_low_callers_no_tests_is_medium() {
        let br = BlastRadius::compute(1, 0);
        assert_eq!(br.risk, RiskLevel::Medium);
        assert!(!br.has_tests);
    }

    #[test]
    fn blast_radius_many_callers_with_tests_is_medium() {
        let br = BlastRadius::compute(5, 2);
        assert_eq!(br.risk, RiskLevel::Medium);
        assert!(br.has_tests);
    }

    #[test]
    fn blast_radius_many_callers_no_tests_is_high() {
        let br = BlastRadius::compute(10, 0);
        assert_eq!(br.risk, RiskLevel::High);
        assert!(!br.has_tests);
    }

    #[test]
    fn blast_radius_serializes_risk_as_snake_case() {
        let br = BlastRadius::compute(5, 0);
        let json = serde_json::to_value(&br).unwrap();
        assert_eq!(json["risk"], "high");
        assert_eq!(json["production_callers"], 5);
        assert_eq!(json["test_callers"], 0);
        assert_eq!(json["has_tests"], false);
    }

    #[test]
    fn blast_radius_boundary_at_three_callers() {
        let low = BlastRadius::compute(2, 1);
        let medium = BlastRadius::compute(3, 1);
        assert_eq!(low.risk, RiskLevel::Low);
        assert_eq!(medium.risk, RiskLevel::Medium);
    }
}
