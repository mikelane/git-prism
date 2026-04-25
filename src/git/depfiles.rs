use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DependencyChange {
    pub name: String,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
pub struct DependencyDiff {
    pub file: String,
    pub added: Vec<DependencyChange>,
    pub removed: Vec<DependencyChange>,
    pub changed: Vec<DependencyChange>,
}

pub fn is_dependency_file(path: &str) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    matches!(
        filename,
        "Cargo.toml" | "package.json" | "go.mod" | "pyproject.toml"
    )
}

pub fn diff_dependencies(path: &str, before: &str, after: &str) -> Option<DependencyDiff> {
    let filename = path.rsplit('/').next().unwrap_or(path);
    match filename {
        "package.json" => Some(diff_package_json(path, before, after)),
        "Cargo.toml" => Some(diff_cargo_toml(path, before, after)),
        "go.mod" => Some(diff_go_mod(path, before, after)),
        "pyproject.toml" => Some(diff_pyproject_toml(path, before, after)),
        _ => None,
    }
}

fn collect_package_json_deps(
    value: &serde_json::Value,
) -> std::collections::HashMap<String, String> {
    let mut deps = std::collections::HashMap::new();
    for section in ["dependencies", "devDependencies"] {
        if let Some(obj) = value.get(section).and_then(|v| v.as_object()) {
            for (name, version) in obj {
                if let Some(v) = version.as_str() {
                    deps.insert(name.clone(), v.to_string());
                }
            }
        }
    }
    deps
}

fn diff_package_json(path: &str, before: &str, after: &str) -> DependencyDiff {
    let before_val: serde_json::Value = serde_json::from_str(before).unwrap_or_default();
    let after_val: serde_json::Value = serde_json::from_str(after).unwrap_or_default();

    let before_deps = collect_package_json_deps(&before_val);
    let after_deps = collect_package_json_deps(&after_val);

    compute_dep_diff(path, &before_deps, &after_deps)
}

fn parse_cargo_toml_deps(content: &str) -> std::collections::HashMap<String, String> {
    let mut deps = std::collections::HashMap::new();
    let mut in_deps_section = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_deps_section = trimmed == "[dependencies]"
                || trimmed == "[dev-dependencies]"
                || trimmed == "[build-dependencies]"
                || trimmed == "[workspace.dependencies]";
            continue;
        }

        if !in_deps_section {
            continue;
        }

        if let Some((name, rest)) = trimmed.split_once('=') {
            let name = name.trim().to_string();
            let rest = rest.trim();
            let version = if rest.starts_with('"') {
                rest.trim_matches('"').to_string()
            } else {
                extract_version_key(rest)
            };
            if !name.is_empty() {
                deps.insert(name, version);
            }
        }
    }
    deps
}

fn extract_version_key(inline_table: &str) -> String {
    // Look for `version = "..."` as a key-value pattern within a TOML inline table.
    // Splits on commas to isolate key-value pairs, then finds the one starting with "version".
    for part in inline_table.split(',') {
        let part = part
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        if let Some((key, val)) = part.split_once('=')
            && key.trim() == "version"
        {
            let val = val.trim().trim_matches('"');
            return val.to_string();
        }
    }
    String::new()
}

fn diff_cargo_toml(path: &str, before: &str, after: &str) -> DependencyDiff {
    let before_deps = parse_cargo_toml_deps(before);
    let after_deps = parse_cargo_toml_deps(after);
    compute_dep_diff(path, &before_deps, &after_deps)
}

fn parse_go_mod_deps(content: &str) -> std::collections::HashMap<String, String> {
    let mut deps = std::collections::HashMap::new();
    let mut in_require = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("require (") || trimmed == "require (" {
            in_require = true;
            continue;
        }

        if in_require && trimmed == ")" {
            in_require = false;
            continue;
        }

        if trimmed.starts_with("require ") && !trimmed.contains('(') {
            let parts: Vec<&str> = trimmed
                .strip_prefix("require ")
                .unwrap()
                .split_whitespace()
                .collect();
            if parts.len() >= 2 {
                deps.insert(parts[0].to_string(), parts[1].to_string());
            }
            continue;
        }

        if in_require {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                deps.insert(parts[0].to_string(), parts[1].to_string());
            }
        }
    }
    deps
}

fn diff_go_mod(path: &str, before: &str, after: &str) -> DependencyDiff {
    let before_deps = parse_go_mod_deps(before);
    let after_deps = parse_go_mod_deps(after);
    compute_dep_diff(path, &before_deps, &after_deps)
}

fn parse_pyproject_deps(content: &str) -> std::collections::HashMap<String, String> {
    let mut deps = std::collections::HashMap::new();
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("dependencies")
            && matches!(
                (trimmed.find('='), trimmed.find('[')),
                (Some(eq), Some(br)) if eq < br
            )
        {
            in_deps = true;
            continue;
        }

        if in_deps && trimmed == "]" {
            in_deps = false;
            continue;
        }

        if in_deps {
            let cleaned = trimmed.trim_matches(|c| c == '"' || c == '\'' || c == ',');
            if !cleaned.is_empty() {
                let (name, version) = split_pep508_spec(cleaned);
                deps.insert(name, version);
            }
        }
    }
    deps
}

fn split_pep508_spec(spec: &str) -> (String, String) {
    for op in &[">=", "<=", "==", "!=", "~=", ">", "<"] {
        if let Some(idx) = spec.find(op) {
            let name = spec[..idx].trim().to_lowercase();
            let version = format!("{}{}", op, spec[idx + op.len()..].trim());
            return (name, version);
        }
    }
    (spec.trim().to_lowercase(), String::new())
}

fn diff_pyproject_toml(path: &str, before: &str, after: &str) -> DependencyDiff {
    let before_deps = parse_pyproject_deps(before);
    let after_deps = parse_pyproject_deps(after);
    compute_dep_diff(path, &before_deps, &after_deps)
}

fn compute_dep_diff(
    path: &str,
    before_deps: &std::collections::HashMap<String, String>,
    after_deps: &std::collections::HashMap<String, String>,
) -> DependencyDiff {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (name, new_ver) in after_deps {
        match before_deps.get(name) {
            None => added.push(DependencyChange {
                name: name.clone(),
                old_version: None,
                new_version: Some(new_ver.clone()),
            }),
            Some(old_ver) if old_ver != new_ver => changed.push(DependencyChange {
                name: name.clone(),
                old_version: Some(old_ver.clone()),
                new_version: Some(new_ver.clone()),
            }),
            _ => {}
        }
    }

    for (name, old_ver) in before_deps {
        if !after_deps.contains_key(name) {
            removed.push(DependencyChange {
                name: name.clone(),
                old_version: Some(old_ver.clone()),
                new_version: None,
            });
        }
    }

    DependencyDiff {
        file: path.to_string(),
        added,
        removed,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_identifies_cargo_toml_as_dependency_file() {
        assert!(is_dependency_file("Cargo.toml"));
        assert!(is_dependency_file("some/path/Cargo.toml"));
    }

    #[test]
    fn it_identifies_package_json_as_dependency_file() {
        assert!(is_dependency_file("package.json"));
    }

    #[test]
    fn it_identifies_go_mod_as_dependency_file() {
        assert!(is_dependency_file("go.mod"));
    }

    #[test]
    fn it_identifies_pyproject_toml_as_dependency_file() {
        assert!(is_dependency_file("pyproject.toml"));
    }

    #[test]
    fn it_rejects_non_dependency_files() {
        assert!(!is_dependency_file("src/main.rs"));
        assert!(!is_dependency_file("README.md"));
        assert!(!is_dependency_file("Cargo.lock"));
    }

    #[test]
    fn it_diffs_package_json_added_dependency() {
        let before = r#"{"dependencies": {"lodash": "^4.17.0"}}"#;
        let after = r#"{"dependencies": {"lodash": "^4.17.0", "express": "^4.18.0"}}"#;
        let result = diff_dependencies("package.json", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "express");
        assert_eq!(result.added[0].new_version.as_deref(), Some("^4.18.0"));
    }

    #[test]
    fn it_diffs_package_json_removed_dependency() {
        let before = r#"{"dependencies": {"lodash": "^4.17.0", "express": "^4.18.0"}}"#;
        let after = r#"{"dependencies": {"lodash": "^4.17.0"}}"#;
        let result = diff_dependencies("package.json", before, after).unwrap();
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].name, "express");
        assert_eq!(result.removed[0].old_version.as_deref(), Some("^4.18.0"));
    }

    #[test]
    fn it_diffs_package_json_changed_version() {
        let before = r#"{"dependencies": {"lodash": "^4.17.0"}}"#;
        let after = r#"{"dependencies": {"lodash": "^4.18.0"}}"#;
        let result = diff_dependencies("package.json", before, after).unwrap();
        assert_eq!(result.changed.len(), 1);
        assert_eq!(result.changed[0].name, "lodash");
        assert_eq!(result.changed[0].old_version.as_deref(), Some("^4.17.0"));
        assert_eq!(result.changed[0].new_version.as_deref(), Some("^4.18.0"));
    }

    #[test]
    fn it_diffs_cargo_toml_added_dependency() {
        let before = "[dependencies]\nserde = \"1.0\"\n";
        let after = "[dependencies]\nserde = \"1.0\"\ntoml = \"0.8\"\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "toml");
        assert_eq!(result.added[0].new_version.as_deref(), Some("0.8"));
    }

    #[test]
    fn it_parses_workspace_dependencies_section() {
        let before = "";
        let after = "[workspace.dependencies]\nserde = \"1.0\"\ntokio = { version = \"1.0\", features = [\"full\"] }\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(result.added.len(), 2);
        let names: Vec<&str> = result.added.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"serde"),
            "should find serde in workspace.dependencies"
        );
        assert!(
            names.contains(&"tokio"),
            "should find tokio in workspace.dependencies"
        );
    }

    #[test]
    fn it_does_not_match_version_inside_url() {
        // A git dependency with a URL containing "version" should not extract garbage
        let before = "";
        let after = "[dependencies]\nmy-crate = { git = \"https://github.com/org/version-manager.git\", branch = \"main\" }\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        // Should be empty string (no version specified), not some substring of the URL
        assert_eq!(
            result.added[0].new_version.as_deref(),
            Some(""),
            "git dep with no version key should have empty version, not URL fragment"
        );
    }

    #[test]
    fn it_extracts_version_after_features() {
        let before = "";
        let after = "[dependencies]\ntokio = { features = [\"full\"], version = \"1.0\" }\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "tokio");
        assert_eq!(result.added[0].new_version.as_deref(), Some("1.0"));
    }

    #[test]
    fn it_diffs_go_mod_added_dependency() {
        let before =
            "module example.com/foo\n\ngo 1.21\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n)\n";
        let after = "module example.com/foo\n\ngo 1.21\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n\tgithub.com/stretchr/testify v1.8.0\n)\n";
        let result = diff_dependencies("go.mod", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "github.com/stretchr/testify");
        assert_eq!(result.added[0].new_version.as_deref(), Some("v1.8.0"));
    }

    #[test]
    fn it_does_not_parse_dependencies_without_equals_before_bracket() {
        // A malformed or unusual line like `dependencies[extras]` starts with
        // "dependencies" and contains "[", but has no "=" before "[".
        // The parser must require the PEP 621 pattern: `dependencies = [`
        let before = "";
        let after = "[project]\nname = \"test\"\ndependencies[extras]\n\"requests>=2.28\",\n]\n";
        let result = diff_dependencies("pyproject.toml", before, after).unwrap();
        assert_eq!(
            result.added.len(),
            0,
            "line without '=' before '[' should not trigger dependency parsing"
        );
    }

    #[test]
    fn it_produces_empty_results_for_poetry_style_pyproject() {
        let before = "";
        let after = r#"[tool.poetry]
name = "my-project"
version = "0.1.0"

[tool.poetry.dependencies]
python = "^3.10"
requests = "^2.28"
click = "^8.0"
"#;
        let result = diff_dependencies("pyproject.toml", before, after).unwrap();
        assert_eq!(
            result.added.len(),
            0,
            "Poetry key-value deps under [tool.poetry.dependencies] are not PEP 621"
        );
    }

    #[test]
    fn it_diffs_pyproject_toml_added_dependency() {
        let before = "[project]\ndependencies = [\n    \"requests>=2.28\",\n]\n";
        let after =
            "[project]\ndependencies = [\n    \"requests>=2.28\",\n    \"click>=8.0\",\n]\n";
        let result = diff_dependencies("pyproject.toml", before, after).unwrap();
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "click");
        assert_eq!(result.added[0].new_version.as_deref(), Some(">=8.0"));
    }

    // --- Gap-closing tests for mutation testing ---

    // Kill mutant: line 73 replace == with != in parse_cargo_toml_deps
    // If [dependencies] == becomes !=, then deps under [dependencies] would NOT be parsed.
    #[test]
    fn it_parses_only_dependencies_section() {
        let content = "[dependencies]\nserde = \"1.0\"\n\n[package]\nname = \"foo\"\n";
        let deps = parse_cargo_toml_deps(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("serde").unwrap(), "1.0");
    }

    // Kill mutant: line 74 replace == with != / replace || with && in parse_cargo_toml_deps
    // Test that [dev-dependencies] alone (without [dependencies]) is parsed.
    #[test]
    fn it_parses_dev_dependencies_section_alone() {
        let content = "[dev-dependencies]\ninsta = \"1.30\"\n";
        let deps = parse_cargo_toml_deps(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("insta").unwrap(), "1.30");
    }

    // Kill mutant: line 74 replace == with != for build-dependencies
    #[test]
    fn it_parses_build_dependencies_section_alone() {
        let content = "[build-dependencies]\ncc = \"1.0\"\n";
        let deps = parse_cargo_toml_deps(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("cc").unwrap(), "1.0");
    }

    // Kill mutant: line 131 replace || with && in parse_go_mod_deps
    // Test "require (" (starts_with variant) and "require (" (exact match variant) separately.
    #[test]
    fn it_parses_go_mod_require_block_with_space_before_paren() {
        // This matches trimmed == "require (" but also starts_with("require (")
        let content =
            "module example.com/foo\n\ngo 1.21\n\nrequire (\n\tgithub.com/pkg/errors v0.9.1\n)\n";
        let deps = parse_go_mod_deps(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("github.com/pkg/errors").unwrap(), "v0.9.1");
    }

    // Kill mutant: line 141 delete ! in parse_go_mod_deps
    // A single-line require without parens: `require github.com/foo v1.0.0`
    // If the `!` is deleted, lines containing '(' would be accepted as single-line requires.
    #[test]
    fn it_parses_go_mod_single_line_require() {
        let content = "module example.com/foo\n\ngo 1.21\n\nrequire github.com/pkg/errors v0.9.1\n";
        let deps = parse_go_mod_deps(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps.get("github.com/pkg/errors").unwrap(), "v0.9.1");
    }

    // Also test that a line like "require (" is NOT treated as single-line require
    // (it should enter block mode, not try to parse "(" as a module path)
    #[test]
    fn it_does_not_treat_require_paren_as_single_line_dep() {
        let content =
            "module example.com/foo\n\ngo 1.21\n\nrequire (\n\tgithub.com/foo/bar v1.0.0\n)\n";
        let deps = parse_go_mod_deps(content);
        // Should find only "github.com/foo/bar", not "(" as a dep name
        assert_eq!(deps.len(), 1);
        assert!(deps.contains_key("github.com/foo/bar"));
        assert!(!deps.contains_key("("));
    }

    // Kill mutant: line 147 replace >= with < in parse_go_mod_deps
    // If >= 2 becomes < 2, single-line requires with exactly 2 parts would NOT be parsed.
    #[test]
    fn it_parses_go_mod_single_line_require_with_exactly_two_parts() {
        let content = "module example.com/foo\n\ngo 1.21\n\nrequire github.com/one v2.0.0\n";
        let deps = parse_go_mod_deps(content);
        assert_eq!(
            deps.len(),
            1,
            "single-line require with 2 parts must be parsed"
        );
        assert_eq!(deps.get("github.com/one").unwrap(), "v2.0.0");
    }

    // Kill mutant: line 235 replace match guard old_ver != new_ver with true
    // If the guard becomes `true`, deps with the same version would appear in `changed`.
    #[test]
    fn it_does_not_report_unchanged_deps_as_changed() {
        let before = "[dependencies]\nserde = \"1.0\"\n";
        let after = "[dependencies]\nserde = \"1.0\"\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(
            result.changed.len(),
            0,
            "same version should not appear in changed"
        );
        assert_eq!(result.added.len(), 0);
        assert_eq!(result.removed.len(), 0);
    }

    // Additional: test compute_dep_diff with a version change to confirm changed works
    #[test]
    fn it_reports_changed_dep_when_version_differs() {
        let before = "[dependencies]\nserde = \"1.0\"\n";
        let after = "[dependencies]\nserde = \"2.0\"\n";
        let result = diff_dependencies("Cargo.toml", before, after).unwrap();
        assert_eq!(result.changed.len(), 1);
        assert_eq!(result.changed[0].name, "serde");
        assert_eq!(result.changed[0].old_version.as_deref(), Some("1.0"));
        assert_eq!(result.changed[0].new_version.as_deref(), Some("2.0"));
    }

    // Kill mutant: line 131 replace || with && in parse_go_mod_deps (the
    // `require (` block-open detector).
    //
    // The intent is "enter block mode for any line that starts with `require (`,
    // including ones with a trailing inline comment". Replacing `||` with `&&`
    // narrows the predicate to "exactly `require (` AND starts with `require (`"
    // i.e. an exact equality, so a `require ( // comment` opener no longer flips
    // `in_require = true` and the deps inside the block are silently dropped.
    //
    // This test exercises that exact case: a `require (` line with a trailing
    // Go-style comment, followed by a single dep, then `)`. Under the original
    // `||`, the dep is parsed; under `&&`, it is not.
    #[test]
    fn it_parses_go_mod_require_block_with_trailing_comment_on_opener() {
        let content = "module example.com/foo\n\ngo 1.21\n\nrequire ( // pinned\n\tgithub.com/pkg/errors v0.9.1\n)\n";
        let deps = parse_go_mod_deps(content);
        assert_eq!(
            deps.len(),
            1,
            "require block opener with trailing comment must enter block mode"
        );
        assert_eq!(deps.get("github.com/pkg/errors").unwrap(), "v0.9.1");
    }
}
