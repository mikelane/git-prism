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
    #[serde(default = "default_true")]
    pub include_function_analysis: bool,
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

fn default_true() -> bool {
    true
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

// --- Tool options (for internal use) ---

#[derive(Debug, Clone)]
pub struct ManifestOptions {
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub include_function_analysis: bool,
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
        assert!(args.include_function_analysis);
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
}
