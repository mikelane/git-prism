//! Import-aware caller scoping.
//!
//! Filters the caller scan in `build_function_context()` to only parse files
//! that plausibly import the changed module. Falls back to full scan for
//! unsupported languages or ambiguous imports.

use std::path::Path;

/// Languages that support import-based scoping.
const SCOPED_LANGUAGES: &[&str] = &["rs", "py", "go", "ts", "tsx", "js", "jsx"];

/// Returns true if the file extension supports import-scoped caller filtering.
pub fn supports_import_scoping(ext: &str) -> bool {
    SCOPED_LANGUAGES.contains(&ext)
}

/// Infer the module path for a file, as it would appear in import statements.
///
/// Returns `None` if the language is unsupported for module path inference.
pub fn infer_module_path(file_path: &str, ext: &str) -> Option<String> {
    match ext {
        "rs" => infer_rust_module(file_path),
        "py" => infer_python_module(file_path),
        "go" => infer_go_module(file_path),
        "ts" | "tsx" | "js" | "jsx" => infer_ts_module(file_path),
        _ => None,
    }
}

/// Check whether a file's import list references the given module path.
pub fn imports_reference_module(
    imports: &[String],
    module_path: &str,
    importer_path: &str,
    ext: &str,
) -> bool {
    match ext {
        "rs" => rust_imports_reference(imports, module_path),
        "py" => python_imports_reference(imports, module_path),
        "go" => go_imports_reference(imports, module_path),
        "ts" | "tsx" | "js" | "jsx" => ts_imports_reference(imports, module_path, importer_path),
        _ => false,
    }
}

/// Check whether two file paths share the same parent directory.
pub fn same_directory(a: &str, b: &str) -> bool {
    let parent_a = Path::new(a).parent();
    let parent_b = Path::new(b).parent();
    match (parent_a, parent_b) {
        (Some(pa), Some(pb)) => pa == pb,
        _ => false,
    }
}

// --- Rust ---

fn infer_rust_module(file_path: &str) -> Option<String> {
    let path = file_path.strip_prefix("src/").unwrap_or(file_path);
    let path = path.strip_suffix(".rs")?;
    let path = path.strip_suffix("/mod").unwrap_or(path);
    Some(format!("crate::{}", path.replace('/', "::")))
}

fn rust_imports_reference(imports: &[String], module_path: &str) -> bool {
    // module_path is like "crate::foo::bar"
    // imports are like "use crate::foo::bar::Thing;"
    let prefix = module_path.strip_prefix("crate::").unwrap_or(module_path);
    imports.iter().any(|imp| {
        let trimmed = imp
            .strip_prefix("use ")
            .unwrap_or(imp)
            .trim_end_matches(';')
            .trim();
        if let Some(path) = trimmed.strip_prefix("crate::") {
            // Check if the import path starts with or equals our module
            path == prefix || path.starts_with(&format!("{prefix}::"))
        } else {
            // super:: and self:: are relative imports — conservatively include
            // these files since they may reference sibling modules
            trimmed.starts_with("super::") || trimmed.starts_with("self::")
        }
    })
}

// --- Python ---

fn infer_python_module(file_path: &str) -> Option<String> {
    let path = file_path.strip_suffix(".py")?;
    let path = path.strip_suffix("/__init__").unwrap_or(path);
    Some(path.replace('/', "."))
}

fn python_imports_reference(imports: &[String], module_path: &str) -> bool {
    imports.iter().any(|imp| {
        if let Some(rest) = imp.strip_prefix("from ") {
            // "from lib import compute" → module is "lib"
            let module = rest.split_whitespace().next().unwrap_or("");
            module == module_path || module.starts_with(&format!("{module_path}."))
        } else if let Some(rest) = imp.strip_prefix("import ") {
            let module = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(',');
            module == module_path || module.starts_with(&format!("{module_path}."))
        } else {
            false
        }
    })
}

// --- Go ---

fn infer_go_module(file_path: &str) -> Option<String> {
    // Go module path is the directory containing the file
    let parent = Path::new(file_path).parent()?;
    let dir = parent.to_str()?;
    if dir.is_empty() {
        return Some(".".to_string());
    }
    Some(dir.to_string())
}

fn go_imports_reference(imports: &[String], module_path: &str) -> bool {
    // Go imports are bare paths like "example/lib" or "fmt"
    // module_path is a directory like "lib" or "internal/parser"
    imports.iter().any(|imp| {
        // Check if import path ends with the module directory
        imp == module_path || imp.ends_with(&format!("/{module_path}"))
    })
}

// --- TypeScript / JavaScript ---

fn infer_ts_module(file_path: &str) -> Option<String> {
    // Strip extension for matching
    let path = file_path
        .strip_suffix(".ts")
        .or_else(|| file_path.strip_suffix(".tsx"))
        .or_else(|| file_path.strip_suffix(".js"))
        .or_else(|| file_path.strip_suffix(".jsx"))?;
    // Strip /index suffix
    let path = path.strip_suffix("/index").unwrap_or(path);
    Some(path.to_string())
}

fn ts_imports_reference(imports: &[String], module_path: &str, importer_path: &str) -> bool {
    let importer_dir = Path::new(importer_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");

    imports.iter().any(|imp| {
        // Extract the module specifier from: import ... from 'specifier';
        let spec = extract_ts_module_specifier(imp);
        let spec = match spec {
            Some(s) => s,
            None => return false,
        };
        // Only handle relative imports (starts with . or ..)
        if !spec.starts_with('.') {
            return false;
        }
        // Resolve relative to importer's directory
        let resolved = resolve_relative_path(importer_dir, &spec);
        resolved == module_path
    })
}

fn extract_ts_module_specifier(import_stmt: &str) -> Option<String> {
    // Find the string between quotes after "from"
    let from_idx = import_stmt.find("from")?;
    let after_from = &import_stmt[from_idx + 4..];
    let quote_char = if after_from.contains('\'') { '\'' } else { '"' };
    let start = after_from.find(quote_char)? + 1;
    let rest = &after_from[start..];
    let end = rest.find(quote_char)?;
    Some(rest[..end].to_string())
}

fn resolve_relative_path(base_dir: &str, relative: &str) -> String {
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        vec![]
    } else {
        base_dir.split('/').collect()
    };

    for segment in relative.split('/') {
        match segment {
            "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }

    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Rust module inference ---

    #[test]
    fn rust_module_from_file() {
        assert_eq!(
            infer_module_path("src/foo/bar.rs", "rs").unwrap(),
            "crate::foo::bar"
        );
    }

    #[test]
    fn rust_module_from_mod_rs() {
        assert_eq!(
            infer_module_path("src/foo/mod.rs", "rs").unwrap(),
            "crate::foo"
        );
    }

    #[test]
    fn rust_module_from_lib() {
        assert_eq!(infer_module_path("src/lib.rs", "rs").unwrap(), "crate::lib");
    }

    // --- Rust import matching ---

    #[test]
    fn rust_import_matches_module() {
        let imports = vec!["use crate::foo::bar::Thing;".to_string()];
        assert!(rust_imports_reference(&imports, "crate::foo::bar"));
    }

    #[test]
    fn rust_import_does_not_match_unrelated() {
        let imports = vec!["use crate::baz::Thing;".to_string()];
        assert!(!rust_imports_reference(&imports, "crate::foo::bar"));
    }

    #[test]
    fn rust_import_exact_module_match() {
        let imports = vec!["use crate::lib::compute;".to_string()];
        assert!(rust_imports_reference(&imports, "crate::lib"));
    }

    // --- Python module inference ---

    #[test]
    fn python_module_from_file() {
        assert_eq!(
            infer_module_path("src/validation.py", "py").unwrap(),
            "src.validation"
        );
    }

    #[test]
    fn python_module_from_init() {
        assert_eq!(
            infer_module_path("utils/__init__.py", "py").unwrap(),
            "utils"
        );
    }

    #[test]
    fn python_module_from_top_level() {
        assert_eq!(infer_module_path("lib.py", "py").unwrap(), "lib");
    }

    // --- Python import matching ---

    #[test]
    fn python_from_import_matches() {
        let imports = vec!["from lib import compute".to_string()];
        assert!(python_imports_reference(&imports, "lib"));
    }

    #[test]
    fn python_import_matches() {
        let imports = vec!["import lib".to_string()];
        assert!(python_imports_reference(&imports, "lib"));
    }

    #[test]
    fn python_import_no_match() {
        let imports = vec!["from other import compute".to_string()];
        assert!(!python_imports_reference(&imports, "lib"));
    }

    // --- Go module inference ---

    #[test]
    fn go_module_from_file() {
        assert_eq!(infer_module_path("lib/lib.go", "go").unwrap(), "lib");
    }

    #[test]
    fn go_module_from_nested() {
        assert_eq!(
            infer_module_path("internal/parser/parser.go", "go").unwrap(),
            "internal/parser"
        );
    }

    // --- Go import matching ---

    #[test]
    fn go_import_matches_suffix() {
        let imports = vec!["example/lib".to_string()];
        assert!(go_imports_reference(&imports, "lib"));
    }

    #[test]
    fn go_import_no_match() {
        let imports = vec!["fmt".to_string()];
        assert!(!go_imports_reference(&imports, "lib"));
    }

    // --- TypeScript module inference ---

    #[test]
    fn ts_module_from_file() {
        assert_eq!(infer_module_path("lib.ts", "ts").unwrap(), "lib");
    }

    #[test]
    fn ts_module_from_nested() {
        assert_eq!(
            infer_module_path("src/utils/helper.ts", "ts").unwrap(),
            "src/utils/helper"
        );
    }

    #[test]
    fn ts_module_from_index() {
        assert_eq!(
            infer_module_path("src/utils/index.ts", "ts").unwrap(),
            "src/utils"
        );
    }

    // --- TypeScript import matching ---

    #[test]
    fn ts_relative_import_matches() {
        let imports = vec!["import { compute } from './lib';".to_string()];
        assert!(ts_imports_reference(&imports, "lib", "importer.ts"));
    }

    #[test]
    fn ts_relative_import_no_match() {
        let imports = vec!["import { compute } from './other';".to_string()];
        assert!(!ts_imports_reference(&imports, "lib", "importer.ts"));
    }

    #[test]
    fn ts_bare_import_never_matches() {
        let imports = vec!["import React from 'react';".to_string()];
        assert!(!ts_imports_reference(&imports, "lib", "importer.ts"));
    }

    #[test]
    fn ts_parent_dir_import_resolves() {
        // ../lib from src/handlers/ resolves to src/lib
        let imports = vec!["import { x } from '../lib';".to_string()];
        assert!(ts_imports_reference(
            &imports,
            "src/lib",
            "src/handlers/api.ts"
        ));
    }

    // --- Module specifier extraction ---

    #[test]
    fn extracts_single_quote_specifier() {
        assert_eq!(
            extract_ts_module_specifier("import { x } from './lib';"),
            Some("./lib".to_string())
        );
    }

    #[test]
    fn extracts_double_quote_specifier() {
        assert_eq!(
            extract_ts_module_specifier("import { x } from \"./lib\";"),
            Some("./lib".to_string())
        );
    }

    // --- Same directory ---

    #[test]
    fn same_directory_detects_match() {
        assert!(same_directory("src/lib.rs", "src/main.rs"));
    }

    #[test]
    fn same_directory_detects_mismatch() {
        assert!(!same_directory("src/lib.rs", "tests/test.rs"));
    }

    // --- supports_import_scoping ---

    #[test]
    fn supported_languages_return_true() {
        for ext in &["rs", "py", "go", "ts", "tsx", "js", "jsx"] {
            assert!(supports_import_scoping(ext), "expected true for {ext}");
        }
    }

    #[test]
    fn unsupported_languages_return_false() {
        for ext in &["rb", "c", "java", "php", "cs", "swift", "kt"] {
            assert!(!supports_import_scoping(ext), "expected false for {ext}");
        }
    }
}
