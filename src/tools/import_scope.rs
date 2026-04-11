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

/// Repo-level context that affects module path inference and import matching.
///
/// Loaded once per `build_function_context()` call. Fields are optional because
/// a mixed-language repo may have some but not others (e.g., a Rust crate has
/// `Cargo.toml` but no `go.mod`).
#[derive(Debug, Clone, Default)]
pub struct RepoContext {
    /// Crate name from `Cargo.toml` `[package] name`. Used to match Rust
    /// integration tests and external-crate imports (`use my_crate::foo;`).
    ///
    /// Cargo package names can contain hyphens, but in Rust source they appear
    /// with underscores (e.g., `git-prism` → `git_prism`). This field stores the
    /// underscore form for direct comparison against import paths.
    pub rust_crate_name: Option<String>,
    /// Module path from `go.mod` `module <path>` directive. Used to match Go
    /// imports whose full path is `<go_module>/<directory>`.
    pub go_module: Option<String>,
}

impl RepoContext {
    /// Load repo context by reading `Cargo.toml` and `go.mod` from the repo root.
    ///
    /// Missing or malformed files produce `None` fields rather than errors —
    /// scoping degrades gracefully to matching based only on file path structure.
    pub fn load(repo_root: &Path) -> Self {
        Self {
            rust_crate_name: read_cargo_crate_name(repo_root),
            go_module: read_go_module_path(repo_root),
        }
    }
}

fn read_cargo_crate_name(repo_root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(repo_root.join("Cargo.toml")).ok()?;
    // Naive TOML scan: find the first `name = "..."` line under [package].
    // This avoids pulling in a TOML parser dependency for one field.
    let mut in_package = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("name")
            && let Some(eq_pos) = rest.find('=')
        {
            let value = rest[eq_pos + 1..].trim();
            let name = value.trim_matches('"').trim_matches('\'');
            if !name.is_empty() {
                // Rust source uses underscores; Cargo package names may have hyphens.
                return Some(name.replace('-', "_"));
            }
        }
    }
    None
}

fn read_go_module_path(repo_root: &Path) -> Option<String> {
    let content = std::fs::read_to_string(repo_root.join("go.mod")).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("module ") {
            let name = rest.trim().trim_matches('"');
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Infer the module path for a file, as it would appear in import statements.
///
/// Returns `None` if the language is unsupported for module path inference.
pub fn infer_module_path(file_path: &str, ext: &str, ctx: &RepoContext) -> Option<String> {
    match ext {
        "rs" => infer_rust_module(file_path),
        "py" => infer_python_module(file_path),
        "go" => infer_go_module(file_path, ctx),
        "ts" | "tsx" | "js" | "jsx" => infer_ts_module(file_path),
        _ => None,
    }
}

/// Check whether a file's import list references the given module path.
///
/// `importer_path` is the path of the file being scanned (the potential caller).
/// `importer_ext` is its extension, used to select the matching logic.
pub fn imports_reference_module(
    imports: &[String],
    module_path: &str,
    importer_path: &str,
    importer_ext: &str,
    ctx: &RepoContext,
) -> bool {
    match importer_ext {
        "rs" => rust_imports_reference(imports, module_path, importer_path, ctx),
        "py" => python_imports_reference(imports, module_path, importer_path),
        "go" => go_imports_reference(imports, module_path, ctx),
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

/// Infer the Rust module path for a file.
///
/// `src/lib.rs` and `src/main.rs` are the crate root and produce `crate`.
/// `src/foo.rs` produces `crate::foo`. `src/foo/mod.rs` produces `crate::foo`.
fn infer_rust_module(file_path: &str) -> Option<String> {
    let path = file_path.strip_suffix(".rs")?;
    // Strip `src/` prefix (standard Cargo layout). Non-standard layouts fall
    // through and produce paths like `crate::crates::foo::bar` which won't
    // match imports but also won't cause false positives.
    let path = path.strip_prefix("src/").unwrap_or(path);
    // Crate root: `src/lib.rs` (library) or `src/main.rs` (binary)
    if path == "lib" || path == "main" {
        return Some("crate".to_string());
    }
    // Module files: `src/foo/mod.rs` → `crate::foo`
    let path = path.strip_suffix("/mod").unwrap_or(path);
    Some(format!("crate::{}", path.replace('/', "::")))
}

/// Strip `use ` or `pub use ` from the start of an import statement.
fn strip_use_prefix(imp: &str) -> &str {
    let trimmed = imp.trim();
    if let Some(rest) = trimmed.strip_prefix("pub use ") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("use ") {
        rest
    } else {
        trimmed
    }
}

fn rust_imports_reference(
    imports: &[String],
    module_path: &str,
    importer_path: &str,
    ctx: &RepoContext,
) -> bool {
    // module_path is like "crate::foo::bar" or "crate" for the crate root.
    let module_tail = module_path.strip_prefix("crate::").unwrap_or(module_path);
    let is_crate_root = module_path == "crate";

    // Compute the importer's own module to resolve `super::` and `self::`.
    let importer_module = infer_rust_module(importer_path);

    imports.iter().any(|imp| {
        let raw = strip_use_prefix(imp).trim_end_matches(';').trim();

        // `crate::foo::bar::Thing` — internal absolute path
        if let Some(path) = raw.strip_prefix("crate::") {
            if is_crate_root {
                // Anything under `crate::` references the crate root.
                return true;
            }
            return path == module_tail
                || path.starts_with(&format!("{module_tail}::"))
                || path.starts_with(&format!("{module_tail}::*"));
        }

        // External crate name form — used by integration tests under `tests/`
        // and by any file that prefers the extern-crate name over `crate::`.
        if let Some(crate_name) = ctx.rust_crate_name.as_deref() {
            let ext_prefix = format!("{crate_name}::");
            if raw == crate_name || raw.starts_with(&ext_prefix) {
                if is_crate_root {
                    return true;
                }
                let path = &raw[ext_prefix.len()..];
                return path == module_tail
                    || path.starts_with(&format!("{module_tail}::"))
                    || path.starts_with(&format!("{module_tail}::*"));
            }
        }

        // Relative imports: resolve against the importer's module path.
        // `use super::foo` from `crate::a::b` means `crate::a::foo`.
        // `use self::foo` from `crate::a::b` means `crate::a::b::foo`.
        if let Some(importer_mod) = importer_module.as_deref() {
            if let Some(rel) = raw.strip_prefix("super::")
                && let Some(resolved) = resolve_rust_super(importer_mod, rel)
            {
                return rust_path_matches(&resolved, module_path, module_tail);
            }
            if let Some(rel) = raw.strip_prefix("self::") {
                let resolved = if importer_mod == "crate" {
                    format!("crate::{rel}")
                } else {
                    format!("{importer_mod}::{rel}")
                };
                return rust_path_matches(&resolved, module_path, module_tail);
            }
        }

        false
    })
}

/// Resolve `super::tail` from an importer module path.
/// E.g., importer `crate::a::b::c`, tail `foo::Bar` → `crate::a::b::foo::Bar`.
fn resolve_rust_super(importer_module: &str, tail: &str) -> Option<String> {
    let parent = importer_module.rsplit_once("::").map(|(p, _)| p)?;
    if parent.is_empty() {
        return None;
    }
    Some(format!("{parent}::{tail}"))
}

/// Check if a resolved path references the changed module.
fn rust_path_matches(resolved: &str, module_path: &str, module_tail: &str) -> bool {
    if module_path == "crate" {
        return resolved == "crate" || resolved.starts_with("crate::");
    }
    let rtail = resolved.strip_prefix("crate::").unwrap_or(resolved);
    rtail == module_tail || rtail.starts_with(&format!("{module_tail}::"))
}

// --- Python ---

fn infer_python_module(file_path: &str) -> Option<String> {
    let path = file_path.strip_suffix(".py")?;
    let path = path.strip_suffix("/__init__").unwrap_or(path);
    Some(path.replace('/', "."))
}

/// Walk up `depth` levels from a dotted module path.
fn python_parent_module(module: &str, depth: usize) -> Option<String> {
    let parts: Vec<&str> = module.split('.').collect();
    if depth >= parts.len() {
        return None;
    }
    let remaining = parts.len() - depth;
    Some(parts[..remaining].join("."))
}

fn python_imports_reference(imports: &[String], module_path: &str, importer_path: &str) -> bool {
    let importer_module = infer_python_module(importer_path);

    imports.iter().any(|imp| {
        let imp = imp.trim();

        // --- `from X import Y` form ---
        if let Some(rest) = imp.strip_prefix("from ") {
            let mut parts = rest.splitn(2, " import ");
            let source = parts.next().unwrap_or("").trim();
            let imported_names = parts.next().unwrap_or("").trim();

            // Handle relative imports: `from . import x`, `from ..pkg import y`
            if source.starts_with('.') {
                let depth = source.chars().take_while(|c| *c == '.').count();
                let suffix = &source[depth..];
                if let Some(importer_mod) = importer_module.as_deref() {
                    let anchor = if suffix.is_empty() {
                        // `from . import x` resolves relative to importer's parent
                        python_parent_module(importer_mod, depth)
                    } else {
                        // `from .sub import x` resolves to parent + suffix
                        python_parent_module(importer_mod, depth).map(|p| {
                            if p.is_empty() {
                                suffix.to_string()
                            } else {
                                format!("{p}.{suffix}")
                            }
                        })
                    };
                    if let Some(resolved) = anchor
                        && python_module_matches(&resolved, imported_names, module_path)
                    {
                        return true;
                    }
                }
                return false;
            }

            // Absolute imports
            return python_module_matches(source, imported_names, module_path);
        }

        // --- `import X` or `import X.Y as Z` form ---
        if let Some(rest) = imp.strip_prefix("import ") {
            // Handle comma-separated: `import os, sys`
            for item in rest.split(',') {
                let item = item.trim();
                // Strip ` as alias` if present
                let module = item
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(',');
                if module == module_path
                    || module.starts_with(&format!("{module_path}."))
                    || module_path.starts_with(&format!("{module}."))
                {
                    return true;
                }
            }
            return false;
        }

        false
    })
}

/// Check if a `from X import Y` form references `module_path`.
///
/// Matches if:
/// - `X` == `module_path` (plain attribute import)
/// - `X.Y` == `module_path` for any `Y` in the imported names (submodule import)
/// - `X` starts with `module_path.` (subpackage of the changed module)
fn python_module_matches(source: &str, imported_names: &str, module_path: &str) -> bool {
    if source == module_path || source.starts_with(&format!("{module_path}.")) {
        return true;
    }
    // Parentheses and trailing commas can appear in multi-line imports.
    let cleaned = imported_names
        .trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace())
        .replace([')', '('], "");
    for name in cleaned.split(',') {
        let name = name.trim();
        // Strip ` as alias`
        let name = name.split_whitespace().next().unwrap_or("");
        if name.is_empty() || name == "*" {
            continue;
        }
        let candidate = format!("{source}.{name}");
        if candidate == module_path {
            return true;
        }
    }
    false
}

// --- Go ---

fn infer_go_module(file_path: &str, ctx: &RepoContext) -> Option<String> {
    let parent = Path::new(file_path).parent()?;
    let dir = parent.to_str()?;
    if let Some(module) = ctx.go_module.as_deref() {
        if dir.is_empty() {
            return Some(module.to_string());
        }
        return Some(format!("{module}/{dir}"));
    }
    // No go.mod: fall back to bare directory path (caller matches by suffix).
    if dir.is_empty() {
        return Some(".".to_string());
    }
    Some(dir.to_string())
}

fn go_imports_reference(imports: &[String], module_path: &str, ctx: &RepoContext) -> bool {
    imports.iter().any(|imp| {
        if ctx.go_module.is_some() {
            // With go.mod, match full import paths exactly.
            imp == module_path
        } else {
            // Fallback: suffix match by directory name.
            imp == module_path || imp.ends_with(&format!("/{module_path}"))
        }
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
        let spec = match extract_ts_module_specifier(imp) {
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

    fn empty_ctx() -> RepoContext {
        RepoContext::default()
    }

    fn rust_ctx() -> RepoContext {
        RepoContext {
            rust_crate_name: Some("git_prism".to_string()),
            go_module: None,
        }
    }

    fn go_ctx() -> RepoContext {
        RepoContext {
            rust_crate_name: None,
            go_module: Some("example.com/foo".to_string()),
        }
    }

    // --- Rust module inference ---

    #[test]
    fn rust_module_from_file() {
        assert_eq!(
            infer_module_path("src/foo/bar.rs", "rs", &empty_ctx()).unwrap(),
            "crate::foo::bar"
        );
    }

    #[test]
    fn rust_module_from_mod_rs() {
        assert_eq!(
            infer_module_path("src/foo/mod.rs", "rs", &empty_ctx()).unwrap(),
            "crate::foo"
        );
    }

    #[test]
    fn rust_module_from_lib_is_crate_root() {
        assert_eq!(
            infer_module_path("src/lib.rs", "rs", &empty_ctx()).unwrap(),
            "crate"
        );
    }

    #[test]
    fn rust_module_from_main_is_crate_root() {
        assert_eq!(
            infer_module_path("src/main.rs", "rs", &empty_ctx()).unwrap(),
            "crate"
        );
    }

    // --- Rust import matching ---

    #[test]
    fn rust_import_matches_module() {
        let imports = vec!["use crate::foo::bar::Thing;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate::foo::bar",
            "src/other.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_import_does_not_match_unrelated() {
        let imports = vec!["use crate::baz::Thing;".to_string()];
        assert!(!rust_imports_reference(
            &imports,
            "crate::foo::bar",
            "src/other.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_import_matches_crate_root() {
        let imports = vec!["use crate::foo;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate",
            "src/other.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_pub_use_is_matched() {
        let imports = vec!["pub use crate::foo::bar::Thing;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate::foo::bar",
            "src/reexport.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_extern_crate_name_matches() {
        let imports = vec!["use git_prism::foo::bar::Thing;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate::foo::bar",
            "tests/integration.rs",
            &rust_ctx()
        ));
    }

    #[test]
    fn rust_extern_crate_name_matches_crate_root() {
        let imports = vec!["use git_prism::compute;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate",
            "tests/integration.rs",
            &rust_ctx()
        ));
    }

    #[test]
    fn rust_super_resolves_against_importer() {
        // Importer is `crate::tools::context` (src/tools/context.rs).
        // `use super::manifest` → `crate::tools::manifest`.
        let imports = vec!["use super::manifest::build;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate::tools::manifest",
            "src/tools/context.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_super_does_not_match_unrelated_sibling() {
        // Importer is `crate::tools::context` but the changed module is
        // `crate::git::reader` — super::manifest does NOT reference it.
        let imports = vec!["use super::manifest::build;".to_string()];
        assert!(!rust_imports_reference(
            &imports,
            "crate::git::reader",
            "src/tools/context.rs",
            &empty_ctx()
        ));
    }

    #[test]
    fn rust_self_resolves_against_importer() {
        // Importer is `crate::tools::context`. `use self::helper` →
        // `crate::tools::context::helper` which is under the importer itself;
        // should match if the importer IS the changed file.
        let imports = vec!["use self::helper;".to_string()];
        assert!(rust_imports_reference(
            &imports,
            "crate::tools::context",
            "src/tools/context.rs",
            &empty_ctx()
        ));
    }

    // --- Python module inference ---

    #[test]
    fn python_module_from_file() {
        assert_eq!(
            infer_module_path("src/validation.py", "py", &empty_ctx()).unwrap(),
            "src.validation"
        );
    }

    #[test]
    fn python_module_from_init() {
        assert_eq!(
            infer_module_path("utils/__init__.py", "py", &empty_ctx()).unwrap(),
            "utils"
        );
    }

    #[test]
    fn python_module_from_top_level() {
        assert_eq!(
            infer_module_path("lib.py", "py", &empty_ctx()).unwrap(),
            "lib"
        );
    }

    // --- Python import matching ---

    #[test]
    fn python_from_import_matches() {
        let imports = vec!["from lib import compute".to_string()];
        assert!(python_imports_reference(&imports, "lib", "importer.py"));
    }

    #[test]
    fn python_import_matches() {
        let imports = vec!["import lib".to_string()];
        assert!(python_imports_reference(&imports, "lib", "importer.py"));
    }

    #[test]
    fn python_import_no_match() {
        let imports = vec!["from other import compute".to_string()];
        assert!(!python_imports_reference(&imports, "lib", "importer.py"));
    }

    #[test]
    fn python_submodule_import_matches_trailing_segment() {
        // `from lib import compute` where the changed file is lib/compute.py
        // (module `lib.compute`). The imported NAME is the submodule.
        let imports = vec!["from lib import compute".to_string()];
        assert!(python_imports_reference(
            &imports,
            "lib.compute",
            "importer.py"
        ));
    }

    #[test]
    fn python_dotted_from_import_matches() {
        // `from pkg.sub import foo` should match changed module `pkg.sub.foo`.
        let imports = vec!["from pkg.sub import foo".to_string()];
        assert!(python_imports_reference(
            &imports,
            "pkg.sub.foo",
            "importer.py"
        ));
    }

    #[test]
    fn python_relative_import_resolves_single_dot() {
        // Importer is `pkg.sub.module`, `from . import sibling` →
        // references `pkg.sub.sibling`.
        let imports = vec!["from . import sibling".to_string()];
        assert!(python_imports_reference(
            &imports,
            "pkg.sub.sibling",
            "pkg/sub/module.py"
        ));
    }

    #[test]
    fn python_relative_import_resolves_dotted_sibling() {
        // `from .sibling import x` from `pkg.sub.module` → `pkg.sub.sibling`.
        let imports = vec!["from .sibling import x".to_string()];
        assert!(python_imports_reference(
            &imports,
            "pkg.sub.sibling",
            "pkg/sub/module.py"
        ));
    }

    #[test]
    fn python_relative_import_resolves_parent() {
        // `from .. import sibling` from `pkg.sub.module` → `pkg.sibling`.
        let imports = vec!["from .. import sibling".to_string()];
        assert!(python_imports_reference(
            &imports,
            "pkg.sibling",
            "pkg/sub/module.py"
        ));
    }

    #[test]
    fn python_relative_import_does_not_match_unrelated() {
        let imports = vec!["from . import sibling".to_string()];
        assert!(!python_imports_reference(
            &imports,
            "other.module",
            "pkg/sub/module.py"
        ));
    }

    // --- Go module inference ---

    #[test]
    fn go_module_from_file_with_go_mod() {
        assert_eq!(
            infer_module_path("internal/parser/parser.go", "go", &go_ctx()).unwrap(),
            "example.com/foo/internal/parser"
        );
    }

    #[test]
    fn go_module_from_file_without_go_mod() {
        assert_eq!(
            infer_module_path("lib/lib.go", "go", &empty_ctx()).unwrap(),
            "lib"
        );
    }

    // --- Go import matching ---

    #[test]
    fn go_import_matches_full_path_with_go_mod() {
        let imports = vec!["example.com/foo/internal/parser".to_string()];
        assert!(go_imports_reference(
            &imports,
            "example.com/foo/internal/parser",
            &go_ctx()
        ));
    }

    #[test]
    fn go_unrelated_external_does_not_match_with_go_mod() {
        // With go.mod set, matching is exact — unrelated external repos that
        // happen to suffix-match the directory should NOT be included.
        let imports = vec!["github.com/unrelated/parser".to_string()];
        assert!(!go_imports_reference(
            &imports,
            "example.com/foo/internal/parser",
            &go_ctx()
        ));
    }

    #[test]
    fn go_import_suffix_matches_without_go_mod() {
        let imports = vec!["example/lib".to_string()];
        assert!(go_imports_reference(&imports, "lib", &empty_ctx()));
    }

    // --- TypeScript module inference ---

    #[test]
    fn ts_module_from_file() {
        assert_eq!(
            infer_module_path("lib.ts", "ts", &empty_ctx()).unwrap(),
            "lib"
        );
    }

    #[test]
    fn ts_module_from_nested() {
        assert_eq!(
            infer_module_path("src/utils/helper.ts", "ts", &empty_ctx()).unwrap(),
            "src/utils/helper"
        );
    }

    #[test]
    fn ts_module_from_index() {
        assert_eq!(
            infer_module_path("src/utils/index.ts", "ts", &empty_ctx()).unwrap(),
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

    // --- RepoContext loading ---

    #[test]
    fn repo_context_reads_cargo_crate_name() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"my-crate\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let ctx = RepoContext::load(dir.path());
        // Hyphens become underscores.
        assert_eq!(ctx.rust_crate_name.as_deref(), Some("my_crate"));
    }

    #[test]
    fn repo_context_handles_missing_cargo_toml() {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = RepoContext::load(dir.path());
        assert!(ctx.rust_crate_name.is_none());
    }

    #[test]
    fn repo_context_reads_go_module_path() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\n\ngo 1.21\n",
        )
        .unwrap();
        let ctx = RepoContext::load(dir.path());
        assert_eq!(ctx.go_module.as_deref(), Some("example.com/foo"));
    }

    #[test]
    fn repo_context_only_reads_package_name_not_dependencies() {
        // A [dependencies] section with name = "..." must not be mistaken
        // for the package name.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"real-crate\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        let ctx = RepoContext::load(dir.path());
        assert_eq!(ctx.rust_crate_name.as_deref(), Some("real_crate"));
    }
}
