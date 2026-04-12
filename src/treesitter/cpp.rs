use super::{
    CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH, body_hash_for_node, sha256_hex,
};
use tree_sitter::Parser;

pub struct CppAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .expect("Error loading C++ grammar");
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

fn function_name_from_declarator(source: &[u8], declarator: &tree_sitter::Node) -> String {
    declarator
        .child_by_field_name("declarator")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string()
}

fn collect_functions(
    node: &tree_sitter::Node,
    source: &[u8],
    scope: &[String],
    functions: &mut Vec<Function>,
    depth: usize,
) {
    if depth >= MAX_RECURSION_DEPTH {
        tracing::warn!(
            depth_limit = MAX_RECURSION_DEPTH,
            "tree-sitter depth guard fired: recursive walk truncated; some functions may be missing"
        );
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "namespace_definition" => {
                let ns_name = child
                    .children(&mut child.walk())
                    .find(|c| c.kind() == "namespace_identifier")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("")
                    .to_string();
                let mut new_scope = scope.to_vec();
                if !ns_name.is_empty() {
                    new_scope.push(ns_name);
                }
                if let Some(body) = child.child_by_field_name("body") {
                    collect_functions(&body, source, &new_scope, functions, depth + 1);
                }
            }
            "class_specifier" | "struct_specifier" => {
                let class_name = child
                    .children(&mut child.walk())
                    .find(|c| c.kind() == "type_identifier")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("")
                    .to_string();
                let mut new_scope = scope.to_vec();
                if !class_name.is_empty() {
                    new_scope.push(class_name);
                }
                if let Some(body) = child.child_by_field_name("body") {
                    collect_functions(&body, source, &new_scope, functions, depth + 1);
                }
            }
            "function_definition" => {
                let raw_name = child
                    .child_by_field_name("declarator")
                    .map(|d| function_name_from_declarator(source, &d))
                    .unwrap_or_default();
                let qualified = if scope.is_empty() {
                    raw_name
                } else {
                    format!("{}::{}", scope.join("::"), raw_name)
                };
                let sig = signature_text(source, &child);
                let body_hash = body_hash_for_node(source, child);
                functions.push(Function {
                    name: qualified,
                    signature: sig,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    body_hash,
                });
            }
            "declaration" => {
                if let Some(declarator) = child.child_by_field_name("declarator")
                    && declarator.kind() == "function_declarator"
                {
                    let raw_name = function_name_from_declarator(source, &declarator);
                    let qualified = if scope.is_empty() {
                        raw_name
                    } else {
                        format!("{}::{}", scope.join("::"), raw_name)
                    };
                    let sig = child.utf8_text(source).unwrap_or("").trim().to_string();
                    let body_hash = sha256_hex(&source[child.start_byte()..child.end_byte()]);
                    functions.push(Function {
                        name: qualified,
                        signature: sig,
                        start_line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
                        body_hash,
                    });
                }
            }
            // linkage_specification body can be declaration_list (braced
            // `extern "C" { ... }`), function_definition (single-def form
            // `extern "C" void foo() {...}`), or declaration (forward-decl
            // form `extern "C" int foo(int);`). Recursing into the
            // linkage_specification itself walks its direct children; the
            // declaration_list arm below handles the braced case.
            "linkage_specification" => {
                collect_functions(&child, source, scope, functions, depth + 1);
            }
            "declaration_list" => {
                collect_functions(&child, source, scope, functions, depth + 1);
            }
            kind if is_preprocessor_container(kind) => {
                collect_functions(&child, source, scope, functions, depth + 1);
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

impl LanguageAnalyzer for CppAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C++ source"))?;
        let mut functions = Vec::new();
        collect_functions(&tree.root_node(), source, &[], &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C++ source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression"
                && let Some(func) = node.child_by_field_name("function")
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match func.kind() {
                    "field_expression" => {
                        let recv = func
                            .child_by_field_name("argument")
                            .and_then(|n| n.utf8_text(source).ok())
                            .map(|s| s.to_string());
                        (true, recv)
                    }
                    _ => (false, None),
                };
                calls.push(CallSite {
                    callee,
                    line: node.start_position().row + 1,
                    is_method_call,
                    receiver,
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C++ source"))?;
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
    fn extracts_class_method() {
        let source = br#"class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator::add");
        assert_eq!(functions[0].signature, "int add(int a, int b)");
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 5);
    }

    #[test]
    fn extracts_namespace_qualified_class_method() {
        let source = br#"namespace math {

class Calculator {
public:
    int add(int a, int b) {
        return a + b;
    }
};

}  // namespace math
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "math::Calculator::add");
    }

    #[test]
    fn extracts_free_function() {
        let source = br#"void free_func(int x) {
    return;
}
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "free_func");
        assert_eq!(functions[0].signature, "void free_func(int x)");
    }

    #[test]
    fn extracts_multiple_methods() {
        let source = br#"class Calc {
public:
    int add(int a, int b) { return a + b; }
    int sub(int a, int b) { return a - b; }
};
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Calc::add");
        assert_eq!(functions[1].name, "Calc::sub");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_include_directives() {
        let source = br#"#include <iostream>
#include <string>
#include "myheader.h"
"#;
        let analyzer = CppAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0], "#include <iostream>");
        assert_eq!(imports[1], "#include <string>");
        assert_eq!(imports[2], "#include \"myheader.h\"");
    }

    #[test]
    fn extracts_function_inside_ifdef() {
        let source = br#"#ifdef SOME_DEFINE
void guarded_func(int x) {
    return;
}
#endif
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "guarded_func");
    }

    #[test]
    fn extracts_class_method_inside_preproc_if() {
        let source = br#"#if defined(PLATFORM_LINUX)
class LinuxImpl {
public:
    void init() {
        // linux init
    }
};
#endif
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "LinuxImpl::init");
    }

    #[test]
    fn extracts_functions_from_nested_preproc_blocks() {
        let source = br#"#ifdef FEATURE_A
#ifdef FEATURE_B
void nested_func() {
    return;
}
#endif
#endif
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "nested_func");
    }

    #[test]
    fn extracts_include_inside_ifdef() {
        let source = br#"#include <iostream>
#ifdef _WIN32
#include <windows.h>
#endif
"#;
        let analyzer = CppAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0], "#include <iostream>");
        assert_eq!(imports[1], "#include <windows.h>");
    }

    #[test]
    fn extracts_include_inside_nested_preproc() {
        let source = br#"#ifdef PLATFORM
#ifdef USE_BOOST
#include <boost/shared_ptr.hpp>
#endif
#endif
"#;
        let analyzer = CppAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0], "#include <boost/shared_ptr.hpp>");
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"void hello() {}
"#;
        let analyzer = CppAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    // Kill line-offset mutants (+ with - or *) by checking exact line numbers.
    #[test]
    fn it_reports_correct_line_numbers_for_function_definition() {
        let source = b"// line 1
// line 2
// line 3
void compute(int x) {
    int y = x + 1;
    return;
}
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 7);
    }

    #[test]
    fn it_reports_correct_line_numbers_for_declaration() {
        let source = b"// line 1
// line 2
void compute(int x);
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 3);
    }

    // Kill "delete match arm declaration" mutant: ensure function declarations
    // (not definitions) are extracted.
    #[test]
    fn it_extracts_function_declaration_without_body() {
        let source = b"int add(int a, int b);
void greet(const char* name);
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].signature, "int add(int a, int b);");
        assert_eq!(functions[1].name, "greet");
    }

    // Kill "replace == with != in collect_functions" for declarator.kind() == "function_declarator"
    #[test]
    fn it_only_extracts_function_declarators_not_variable_declarations() {
        let source = b"int x = 42;
void foo(int a);
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "foo");
    }

    // Kill is_preprocessor_container -> true mutant
    #[test]
    fn it_extracts_standalone_function_not_in_preproc() {
        let source = b"void standalone() {
    return;
}
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "standalone");
    }

    // Kill match guard with true for imports: ensure includes inside
    // preproc_else and preproc_elif are also found (tests more container kinds)
    #[test]
    fn it_extracts_include_inside_preproc_else() {
        let source = b"#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif
";
        let analyzer = CppAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0], "#include <windows.h>");
        assert_eq!(imports[1], "#include <unistd.h>");
    }

    // Kill match guard with true for functions in preproc containers
    #[test]
    fn it_extracts_function_inside_preproc_else() {
        let source = b"#ifdef _WIN32
void win_init() { return; }
#else
void unix_init() { return; }
#endif
";
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "win_init");
        assert_eq!(functions[1].name, "unix_init");
    }

    #[test]
    fn extracts_function_and_method_calls() {
        let source = br#"void process() {
    int x = calculate(input);
    v.push_back(42);
    auto result = std::make_unique<Foo>(x);
}
"#;
        let analyzer = CppAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"v.push_back"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = CppAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Security regression: deeply-nested namespace declarations used to stack-overflow
    /// `collect_functions` because it recursed without a depth limit. An attacker committing
    /// a C++ file with thousands of nested namespaces could crash git-prism during
    /// `get_change_manifest`. The analyzer must now complete without crashing.
    ///
    /// Runs on a thread with a 2 MB stack: roomy enough for bounded recursion to
    /// `MAX_RECURSION_DEPTH` but far too small for unbounded recursion to 5000 frames.
    #[test]
    fn it_completes_without_overflow_on_deeply_nested_namespaces() {
        const GENERATED_NESTING_LEVELS: usize = 5000;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = CppAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        let functions = result.expect("analyzer must return Ok on deeply-nested input");
        // Namespaces contain no functions themselves — everything past the cap is guarded out.
        assert!(functions.is_empty());
    }

    /// Triangulation: 255 nested namespaces with a function at the innermost level.
    /// The guard fires at depth 256, so depth 255 must still allow extraction.
    #[test]
    fn it_extracts_functions_at_boundary_nesting_depth() {
        const GENERATED_NESTING_LEVELS: usize = 255;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        source.push_str("void leaf_fn() {}\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        assert_eq!(
            functions.len(),
            1,
            "function at depth 255 must be extracted"
        );
        assert!(functions[0].name.ends_with("leaf_fn"));
    }

    #[test]
    fn extracts_functions_inside_extern_c() {
        let source = br#"extern "C" {

void ffi_init() {
    printf("init\n");
}

void ffi_cleanup() {
    printf("cleanup\n");
}

}
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "ffi_init");
        assert_eq!(functions[1].name, "ffi_cleanup");
    }

    // Single-declaration extern "C" (no braces) — linkage_specification wraps
    // a bare function_definition instead of a declaration_list with body field.
    #[test]
    fn extracts_function_inside_single_extern_c() {
        let source = br#"extern "C" int compute(int x) {
    return x + 1;
}
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "compute");
    }

    #[test]
    fn extracts_functions_inside_nested_extern_c() {
        let source = br#"#ifdef __cplusplus
extern "C" {
#endif

void inner_fn() {
    return;
}

#ifdef __cplusplus
}
#endif
"#;
        let analyzer = CppAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "inner_fn");
    }

    /// Depth-guard warning: deeply-nested namespaces must emit the warning when truncated.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_namespaces() {
        const NESTING_DEPTH: usize = 300;

        let mut source = String::new();
        for i in 0..NESTING_DEPTH {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        for _ in 0..NESTING_DEPTH {
            source.push_str("}\n");
        }

        let analyzer = CppAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_functions() {
        let source = b"void foo() {}\nvoid bar() {}\n";
        let analyzer = CppAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Depth-guard warning: deeply-nested preprocessor blocks in collect_imports must emit warning.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_preproc_in_imports() {
        const NESTING_DEPTH: usize = 300;

        let mut source = String::new();
        for i in 0..NESTING_DEPTH {
            source.push_str(&format!("#ifdef MACRO_{i}\n"));
        }
        source.push_str("#include <deep.h>\n");
        for _ in 0..NESTING_DEPTH {
            source.push_str("#endif\n");
        }

        let analyzer = CppAnalyzer;
        let _ = analyzer.extract_imports(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
    }

    /// Triangulation: shallow import input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_imports() {
        let source = b"#include <stdio.h>\n#include <stdlib.h>\n";
        let analyzer = CppAnalyzer;
        let _ = analyzer.extract_imports(source);
        assert!(!logs_contain("depth guard fired"));
    }
}
