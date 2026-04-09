use std::path::Path;

use chrono::Utc;

use crate::git::reader::RepoReader;
use crate::tools::manifest::build_manifest;
use crate::tools::types::{
    CalleeEntry, CallerEntry, ContextMetadata, FunctionChangeType, FunctionContextEntry,
    FunctionContextResponse, ManifestOptions, ToolError,
};
use crate::treesitter::analyzer_for_extension;

fn extension_from_path(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .filter(|ext| path.len() > ext.len() + 1)
        .unwrap_or("")
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || lower.contains("test_")
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.jsx")
        || lower.ends_with("_spec.rb")
        || lower.ends_with("test.java")
        || lower.ends_with("tests.cs")
}

/// Build function context for changed functions in a commit range.
///
/// For each changed function in the manifest:
/// 1. Extract its callees from the head version of its file
/// 2. Scan all files in the repo at head_ref for callers
/// 3. Classify callers as test references based on path conventions
pub fn build_function_context(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<FunctionContextResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;
    let base_commit = reader.resolve_commit(base_ref)?;
    let head_commit = reader.resolve_commit(head_ref)?;

    // Step 1: Get the manifest to find changed functions
    let options = ManifestOptions {
        include_patterns: vec![],
        exclude_patterns: vec![],
        include_function_analysis: true,
    };
    let manifest = build_manifest(repo_path, base_ref, head_ref, &options, 0, 10_000)?;

    // Collect changed functions with their file paths
    let mut changed_functions: Vec<(String, String, FunctionChangeType)> = Vec::new();
    for file in &manifest.files {
        if let Some(ref fns) = file.functions_changed {
            for fc in fns {
                changed_functions.push((
                    fc.name.clone(),
                    file.path.clone(),
                    fc.change_type.clone(),
                ));
            }
        }
    }

    // Step 2: List all files in the repo at head_ref for caller scanning
    let all_files = reader.list_files_at_ref(head_ref)?;

    // Step 3: For each file with a supported language, parse and extract calls
    let mut file_calls: Vec<(
        String,
        Vec<crate::treesitter::CallSite>,
        Vec<crate::treesitter::Function>,
    )> = Vec::new();
    for file_path in &all_files {
        let ext = extension_from_path(file_path);
        if let Some(analyzer) = analyzer_for_extension(ext)
            && let Ok(content) = reader.read_file_at_ref(head_ref, file_path)
        {
            let calls = analyzer
                .extract_calls(content.as_bytes())
                .unwrap_or_default();
            let functions = analyzer
                .extract_functions(content.as_bytes())
                .unwrap_or_default();
            if !calls.is_empty() || !functions.is_empty() {
                file_calls.push((file_path.clone(), calls, functions));
            }
        }
    }

    // Step 4: Build context for each changed function
    let mut function_entries: Vec<FunctionContextEntry> = Vec::new();

    for (func_name, func_file, change_type) in &changed_functions {
        // Extract callees: calls made BY this function in the head version
        let callees = extract_callees_for_function(&reader, head_ref, func_file, func_name);

        // Find callers: other files (and same file) that call this function
        let mut callers = Vec::new();
        let mut test_references = Vec::new();

        let leaf_name = leaf_function_name(func_name);

        for (caller_file, calls, functions) in &file_calls {
            for call in calls {
                let call_leaf = leaf_function_name(&call.callee);
                if call_leaf == leaf_name {
                    // Find which function contains this call
                    let containing_fn = find_containing_function(functions, call.line);
                    let is_test = is_test_path(caller_file);

                    let entry = CallerEntry {
                        file: caller_file.clone(),
                        line: call.line,
                        caller: containing_fn.unwrap_or_default(),
                        is_test,
                    };

                    if is_test {
                        test_references.push(entry);
                    } else {
                        callers.push(entry);
                    }
                }
            }
        }

        let caller_count = callers.len() + test_references.len();

        function_entries.push(FunctionContextEntry {
            name: func_name.clone(),
            file: func_file.clone(),
            change_type: change_type.clone(),
            callers,
            callees,
            test_references,
            caller_count,
        });
    }

    Ok(FunctionContextResponse {
        metadata: ContextMetadata {
            base_ref: base_ref.to_string(),
            head_ref: head_ref.to_string(),
            base_sha: base_commit.sha,
            head_sha: head_commit.sha,
            generated_at: Utc::now(),
        },
        functions: function_entries,
    })
}

/// Extract the callees (functions called) from a specific function's body.
fn extract_callees_for_function(
    reader: &RepoReader,
    head_ref: &str,
    file_path: &str,
    func_name: &str,
) -> Vec<CalleeEntry> {
    let ext = extension_from_path(file_path);
    let analyzer = match analyzer_for_extension(ext) {
        Some(a) => a,
        None => return vec![],
    };
    let content = match reader.read_file_at_ref(head_ref, file_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let functions = analyzer
        .extract_functions(content.as_bytes())
        .unwrap_or_default();
    let calls = analyzer
        .extract_calls(content.as_bytes())
        .unwrap_or_default();

    // Find the function by name
    let func = match functions.iter().find(|f| f.name == func_name) {
        Some(f) => f,
        None => return vec![],
    };

    // Filter calls to those within this function's line range
    calls
        .iter()
        .filter(|c| c.line >= func.start_line && c.line <= func.end_line)
        .map(|c| CalleeEntry {
            callee: c.callee.clone(),
            line: c.line,
        })
        .collect()
}

/// Extract the "leaf" name from a potentially qualified function name.
/// "std::collections::HashMap::new" → "new"
/// "server.start" → "start"
/// "foo" → "foo"
fn leaf_function_name(name: &str) -> &str {
    name.rsplit_once("::")
        .or_else(|| name.rsplit_once('.'))
        .map(|(_, leaf)| leaf)
        .unwrap_or(name)
}

/// Find which function contains a given line number.
fn find_containing_function(
    functions: &[crate::treesitter::Function],
    line: usize,
) -> Option<String> {
    functions
        .iter()
        .find(|f| line >= f.start_line && line <= f.end_line)
        .map(|f| f.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_context_test_repo() -> (TempDir, std::path::PathBuf) {
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
            .args(["config", "user.name", "Test"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Commit 1: lib.rs with calculate, main.rs calling it, test file
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::create_dir_all(path.join("tests")).unwrap();
        std::fs::write(
            path.join("src/lib.rs"),
            "pub fn calculate(x: i32) -> i32 {\n    x + 1\n}\n\npub fn helper(x: i32) -> i32 {\n    x * 2\n}\n",
        ).unwrap();
        std::fs::write(
            path.join("src/main.rs"),
            "fn main() {\n    let result = calculate(42);\n}\n",
        )
        .unwrap();
        std::fs::write(
            path.join("tests/test_lib.rs"),
            "fn test_calculate() {\n    let result = calculate(1);\n}\n",
        )
        .unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Commit 2: modify calculate body
        std::fs::write(
            path.join("src/lib.rs"),
            "pub fn calculate(x: i32) -> i32 {\n    x + 1 + helper(x)\n}\n\npub fn helper(x: i32) -> i32 {\n    x * 3\n}\n\npub fn process(data: i32) -> i32 {\n    calculate(data) + helper(data)\n}\n",
        ).unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "modify calculate, add process"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_returns_context_for_changed_functions() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();
        assert!(!result.functions.is_empty());
    }

    #[test]
    fn it_finds_callers_in_other_files() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let calculate_ctx = result
            .functions
            .iter()
            .find(|f| f.name == "calculate")
            .expect("calculate should have context");

        // main.rs calls calculate
        let has_main_caller = calculate_ctx
            .callers
            .iter()
            .any(|c| c.file.contains("main.rs"));
        assert!(
            has_main_caller,
            "calculate should have a caller in main.rs, got: {:?}",
            calculate_ctx.callers
        );
    }

    #[test]
    fn it_finds_test_references() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let calculate_ctx = result
            .functions
            .iter()
            .find(|f| f.name == "calculate")
            .expect("calculate should have context");

        assert!(
            !calculate_ctx.test_references.is_empty(),
            "calculate should have test references, got: {:?}",
            calculate_ctx.test_references
        );
        assert!(
            calculate_ctx.test_references[0].file.contains("test"),
            "test reference should be in a test file"
        );
    }

    #[test]
    fn it_extracts_callees_for_changed_functions() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let process_ctx = result.functions.iter().find(|f| f.name == "process");

        if let Some(ctx) = process_ctx {
            let callee_names: Vec<&str> = ctx.callees.iter().map(|c| c.callee.as_str()).collect();
            assert!(
                callee_names.contains(&"calculate"),
                "process should call calculate, got: {:?}",
                callee_names
            );
        }
    }

    #[test]
    fn it_includes_metadata() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        assert_eq!(result.metadata.base_ref, "HEAD~1");
        assert_eq!(result.metadata.head_ref, "HEAD");
        assert!(!result.metadata.base_sha.is_empty());
        assert!(!result.metadata.head_sha.is_empty());
    }

    #[test]
    fn leaf_function_name_extracts_simple() {
        assert_eq!(leaf_function_name("foo"), "foo");
    }

    #[test]
    fn leaf_function_name_extracts_from_scoped() {
        assert_eq!(leaf_function_name("std::collections::HashMap::new"), "new");
    }

    #[test]
    fn leaf_function_name_extracts_from_dotted() {
        assert_eq!(leaf_function_name("server.start"), "start");
    }

    #[test]
    fn is_test_path_detects_test_directories() {
        assert!(is_test_path("tests/test_lib.rs"));
        assert!(is_test_path("src/__tests__/foo.test.ts"));
        assert!(is_test_path("test/helper_test.go"));
    }

    #[test]
    fn is_test_path_rejects_production_paths() {
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("src/server.py"));
        assert!(!is_test_path("pkg/handler.go"));
    }
}
