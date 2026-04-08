use super::{Function, LanguageAnalyzer, sha256_hex};
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

fn collect_functions(node: &tree_sitter::Node, source: &[u8], functions: &mut Vec<Function>) {
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
                let body_hash = {
                    let body_node = child.child_by_field_name("body").unwrap_or(child);
                    sha256_hex(&source[body_node.start_byte()..body_node.end_byte()])
                };
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
                collect_functions(&child, source, functions);
            }
            _ => {}
        }
    }
}

fn collect_imports(node: &tree_sitter::Node, source: &[u8], imports: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "preproc_include" => {
                let text = child.utf8_text(source).unwrap_or("").trim().to_string();
                imports.push(text);
            }
            kind if is_preprocessor_container(kind) => {
                collect_imports(&child, source, imports);
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
        collect_functions(&tree.root_node(), source, &mut functions);
        Ok(functions)
    }

    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C source"))?;
        let mut imports = Vec::new();
        collect_imports(&tree.root_node(), source, &mut imports);
        Ok(imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
