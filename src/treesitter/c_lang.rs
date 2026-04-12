use super::{
    CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH, body_hash_for_node, sha256_hex,
};
use tree_sitter::Parser;

pub struct CAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("Error loading C grammar");
    parser
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
    let start = node.start_byte();
    let body = node.child_by_field_name("body");
    let end = body.map_or(node.end_byte(), |b| b.start_byte());
    let raw = &source[start..end];
    String::from_utf8_lossy(raw).trim().to_string()
}

// Non-preprocessor nodes don't contain function/import children in the AST;
// unconditional recursion produces the same result.
fn is_preprocessor_container(kind: &str) -> bool {
    matches!(
        kind,
        "preproc_ifdef" | "preproc_if" | "preproc_else" | "preproc_elif"
    )
}

fn collect_functions(
    node: &tree_sitter::Node,
    source: &[u8],
    functions: &mut Vec<Function>,
    depth: usize,
) {
    if depth >= MAX_RECURSION_DEPTH {
        tracing::warn!(
            depth_limit = MAX_RECURSION_DEPTH,
            language = "c",
            operation = "functions",
            "tree-sitter depth guard fired: recursive walk truncated; some functions may be missing"
        );
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let name = child
                    .child_by_field_name("declarator")
                    .and_then(|d| d.child_by_field_name("declarator"))
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("")
                    .to_string();
                let signature = signature_text(source, &child);
                let body_hash = body_hash_for_node(source, child);
                functions.push(Function {
                    name,
                    signature,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    body_hash,
                });
            }
            "declaration" => {
                if let Some(declarator) = child.child_by_field_name("declarator")
                    && declarator.kind() == "function_declarator"
                {
                    let name = declarator
                        .child_by_field_name("declarator")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("")
                        .to_string();
                    let signature = child.utf8_text(source).unwrap_or("").trim().to_string();
                    let body_hash = sha256_hex(&source[child.start_byte()..child.end_byte()]);
                    functions.push(Function {
                        name,
                        signature,
                        start_line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
                        body_hash,
                    });
                }
            }
            kind if is_preprocessor_container(kind) => {
                collect_functions(&child, source, functions, depth + 1);
            }
            _ => {}
        }
    }
}

fn collect_imports(
    node: &tree_sitter::Node,
    source: &[u8],
    imports: &mut Vec<String>,
    depth: usize,
) {
    if depth >= MAX_RECURSION_DEPTH {
        tracing::warn!(
            depth_limit = MAX_RECURSION_DEPTH,
            language = "c",
            operation = "imports",
            "tree-sitter depth guard fired: recursive walk truncated; some imports may be missing"
        );
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "preproc_include" => {
                let text = child.utf8_text(source).unwrap_or("").trim().to_string();
                imports.push(text);
            }
            kind if is_preprocessor_container(kind) => {
                collect_imports(&child, source, imports, depth + 1);
            }
            _ => {}
        }
    }
}

impl LanguageAnalyzer for CAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C source"))?;
        let mut functions = Vec::new();
        collect_functions(&tree.root_node(), source, &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression"
                && let Some(func) = node.child_by_field_name("function")
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                calls.push(CallSite {
                    callee,
                    line: node.start_position().row + 1,
                    is_method_call: false,
                    receiver: None,
                });
            }
            for i in (0..node.child_count()).rev() {
                if let Some(child) = node.child(i as u32) {
                    stack.push(child);
                }
            }
        }
        calls.sort_by_key(|c| c.line);
        Ok(calls)
    }

    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C source"))?;
        let mut imports = Vec::new();
        collect_imports(&tree.root_node(), source, &mut imports, 0);
        Ok(imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    fn extracts_multiple_functions() {
        let source = br#"int add(int a, int b) {
    return a + b;
}

void greet(const char* name) {
    printf("Hello, %s!\n", name);
}
"#;
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].signature, "int add(int a, int b)");
        assert_eq!(functions[1].name, "greet");
    }

    #[test]
    fn extracts_function_with_params_and_return_type() {
        let source = br#"double compute(int x, float y) {
    return (double)x + y;
}
"#;
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "compute");
        assert_eq!(functions[0].signature, "double compute(int x, float y)");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_function_declarations_from_header() {
        let source = br#"#ifndef UTILS_H
#define UTILS_H

int add(int a, int b);
int multiply(int a, int b);
void greet(const char* name);

#endif
"#;
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 3);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].signature, "int add(int a, int b);");
        assert_eq!(functions[1].name, "multiply");
        assert_eq!(functions[2].name, "greet");
    }

    #[test]
    fn extracts_include_directives() {
        let source = br#"#include <stdio.h>
#include <stdlib.h>
#include "myheader.h"
"#;
        let analyzer = CAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0], "#include <stdio.h>");
        assert_eq!(imports[1], "#include <stdlib.h>");
        assert_eq!(imports[2], "#include \"myheader.h\"");
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"void hello(void) {
}
"#;
        let analyzer = CAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn extracts_simple_function() {
        let source = br#"#include <stdio.h>

void hello(void) {
    printf("hello\n");
}
"#;
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "hello");
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 5);
    }

    // Kill line-offset mutants (+ with - or *) by checking exact line numbers
    // on a multi-line function that does NOT start on line 1.
    #[test]
    fn it_reports_correct_line_numbers_for_multiline_function() {
        let source = b"// comment line 1
// comment line 2
// comment line 3
int compute(int a, int b) {
    int c = a + b;
    return c * 2;
}
";
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 7);
    }

    #[test]
    fn it_reports_correct_line_numbers_for_declaration() {
        let source = b"// line 1
// line 2
int add(int a, int b);
";
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 3);
    }

    // Kill is_preprocessor_container -> true mutant: a non-preprocessor node
    // (like a regular function) should still be extracted. If is_preprocessor_container
    // always returns true, non-matching kinds would recurse instead of falling to _.
    #[test]
    fn it_extracts_function_not_inside_ifdef() {
        let source = b"void standalone(void) {
    return;
}
";
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "standalone");
    }

    // Kill match guard is_preprocessor_container(kind) with true for functions:
    // If guard is always true, ALL non-matching kinds recurse into children
    // instead of being skipped. We need a function inside #ifdef to ensure
    // the preprocessor recursion path works correctly.
    #[test]
    fn it_extracts_function_inside_ifdef() {
        let source = b"#ifdef FEATURE_X
void guarded(int x) {
    return;
}
#endif
";
        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "guarded");
    }

    // Kill match guard for imports: if guard always true, includes inside
    // #ifdef would be incorrectly handled.
    #[test]
    fn it_extracts_include_inside_ifdef() {
        let source = b"#include <stdio.h>
#ifdef _WIN32
#include <windows.h>
#endif
";
        let analyzer = CAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0], "#include <stdio.h>");
        assert_eq!(imports[1], "#include <windows.h>");
    }

    // Kill match guard with false: if guard is always false, includes inside
    // preprocessor blocks would NOT be found.
    #[test]
    fn it_extracts_include_inside_nested_preproc() {
        let source = b"#ifdef PLATFORM
#ifdef USE_LIB
#include <special.h>
#endif
#endif
";
        let analyzer = CAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0], "#include <special.h>");
    }

    /// Security regression: deeply-nested preprocessor blocks used to stack-overflow
    /// `collect_functions` because it recursed without a depth limit. An attacker committing
    /// a C file with thousands of nested `#ifdef` blocks could crash git-prism during
    /// `get_change_manifest`. The analyzer must now complete without crashing.
    ///
    /// Runs on a thread with a 2 MB stack: roomy enough for bounded recursion to
    /// `MAX_RECURSION_DEPTH` but far too small for unbounded recursion to 5000 frames.
    #[test]
    fn it_completes_without_overflow_on_deeply_nested_preproc_blocks() {
        const GENERATED_NESTING_LEVELS: usize = 5000;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("#ifdef MACRO_{i}\n"));
        }
        // Function is past the cap — it should not be extracted.
        source.push_str("void deep_fn(void) {}\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("#endif\n");
        }

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = CAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        let functions = result.expect("analyzer must return Ok on deeply-nested input");
        // Guard fires at depth 256 — the function at depth 5000 is not extracted.
        assert!(functions.is_empty());
    }

    /// Triangulation: 255 nested `#ifdef` blocks with a function at the innermost level.
    /// The guard fires at depth 256, so depth 255 must still allow extraction.
    #[test]
    fn it_extracts_functions_at_boundary_nesting_depth() {
        const GENERATED_NESTING_LEVELS: usize = 255;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("#ifdef MACRO_{i}\n"));
        }
        source.push_str("void leaf_fn(void) {}\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("#endif\n");
        }

        let analyzer = CAnalyzer;
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        assert_eq!(
            functions.len(),
            1,
            "function at depth 255 must be extracted"
        );
        assert_eq!(functions[0].name, "leaf_fn");
    }

    #[test]
    fn extracts_function_calls() {
        let source = br#"void process() {
    int x = calculate(input);
    printf("result: %d\n", x);
    free(ptr);
}
"#;
        let analyzer = CAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"printf"));
        assert!(callees.contains(&"free"));
        assert!(calls.iter().all(|c| !c.is_method_call));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = CAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Depth-guard warning: deeply-nested preprocessor blocks must emit the warning when truncated.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_preproc_blocks() {
        const GENERATED_NESTING_LEVELS: usize = 300;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("#ifdef MACRO_{i}\n"));
        }
        source.push_str("void deep_fn(void) {}\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("#endif\n");
        }

        let analyzer = CAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
        assert!(logs_contain("language=\"c\""));
        assert!(logs_contain("operation=\"functions\""));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_functions() {
        let source = b"void foo(void) {}\nvoid bar(void) {}\n";
        let analyzer = CAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Depth-guard warning: deeply-nested preprocessor blocks in collect_imports must emit warning.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_preproc_in_imports() {
        const GENERATED_NESTING_LEVELS: usize = 300;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("#ifdef MACRO_{i}\n"));
        }
        source.push_str("#include <deep.h>\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("#endif\n");
        }

        let analyzer = CAnalyzer;
        let _ = analyzer.extract_imports(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
        assert!(logs_contain("language=\"c\""));
        assert!(logs_contain("operation=\"imports\""));
    }

    /// Triangulation: shallow import input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_imports() {
        let source = b"#include <stdio.h>\n#include <stdlib.h>\n";
        let analyzer = CAnalyzer;
        let _ = analyzer.extract_imports(source);
        assert!(!logs_contain("depth guard fired"));
    }
}
