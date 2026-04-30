use super::{CallSite, Function, LanguageAnalyzer, body_hash_for_node};
use tree_sitter::Parser;

pub struct GoAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .expect("Error loading Go grammar");
    parser
}

fn extract_receiver_type(source: &[u8], node: &tree_sitter::Node) -> String {
    let receiver = match node.child_by_field_name("receiver") {
        Some(r) => r,
        None => return String::new(),
    };
    // The receiver is a parameter_list; find the type inside it
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration"
            && let Some(type_node) = child.child_by_field_name("type")
        {
            let type_text = type_node.utf8_text(source).unwrap_or("");
            return type_text.trim_start_matches('*').to_string();
        }
    }
    String::new()
}

fn import_path_from_spec(source: &[u8], spec: &tree_sitter::Node) -> Option<String> {
    let path_node = spec.child_by_field_name("path")?;
    let path = path_node.utf8_text(source).ok()?;
    Some(path.trim_matches('"').to_string())
}

fn collect_import_paths(source: &[u8], node: &tree_sitter::Node, imports: &mut Vec<String>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "import_spec" => {
                if let Some(path) = import_path_from_spec(source, &child) {
                    imports.push(path);
                }
            }
            "import_spec_list" => {
                let mut inner = child.walk();
                for spec in child.children(&mut inner) {
                    if spec.kind() == "import_spec"
                        && let Some(path) = import_path_from_spec(source, &spec)
                    {
                        imports.push(path);
                    }
                }
            }
            _ => {}
        }
    }
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
    let start = node.start_byte();
    let body = node.child_by_field_name("body");
    let end = body.map_or(node.end_byte(), |b| b.start_byte());
    let raw = &source[start..end];
    String::from_utf8_lossy(raw).trim().to_string()
}

impl LanguageAnalyzer for GoAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_declaration" => {
                    let name = child
                        .child_by_field_name("name")
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
                "method_declaration" => {
                    let method_name = child
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("");
                    let receiver_type = extract_receiver_type(source, &child);
                    let name = format!("{receiver_type}.{method_name}");
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
                _ => {}
            }
        }

        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression"
                && let Some(func) = node.child_by_field_name("function")
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match func.kind() {
                    "selector_expression" => {
                        let recv = func
                            .child_by_field_name("operand")
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "import_declaration" {
                collect_import_paths(source, &child, &mut imports);
            }
        }

        Ok(imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_simple_function() {
        let source = br#"package main

func hello() {
    fmt.Println("hello")
}
"#;
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "hello");
        assert_eq!(functions[0].signature, "func hello()");
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 5);
    }

    #[test]
    fn extracts_method_with_receiver() {
        let source = br#"package main

func (s *Server) Handle(w http.ResponseWriter, r *http.Request) {
    w.Write([]byte("ok"))
}
"#;
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Server.Handle");
        assert_eq!(
            functions[0].signature,
            "func (s *Server) Handle(w http.ResponseWriter, r *http.Request)"
        );
    }

    #[test]
    fn extracts_multiple_functions() {
        let source = br#"package main

func foo() int {
    return 1
}

func bar(x int, y int) string {
    return "hello"
}
"#;
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "foo");
        assert_eq!(functions[0].signature, "func foo() int");
        assert_eq!(functions[1].name, "bar");
        assert_eq!(functions[1].signature, "func bar(x int, y int) string");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"package main\n";
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_single_import() {
        let source = br#"package main

import "fmt"
"#;
        let analyzer = GoAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["fmt"]);
    }

    #[test]
    fn extracts_grouped_imports() {
        let source = br#"package main

import (
    "fmt"
    "os"
    "net/http"
)
"#;
        let analyzer = GoAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["fmt", "os", "net/http"]);
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = b"package main\n";
        let analyzer = GoAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    // Kill line-offset mutants (+ with - or *) for function_declaration
    #[test]
    fn it_reports_correct_line_numbers_for_function() {
        let source = b"package main

// comment line 3
// comment line 4
func compute(a int, b int) int {
    c := a + b
    return c * 2
}
";
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 5);
        assert_eq!(functions[0].end_line, 8);
    }

    // Kill line-offset mutants for method_declaration
    #[test]
    fn it_reports_correct_line_numbers_for_method() {
        let source = b"package main

// line 3
// line 4
// line 5
func (s *Server) Start(port int) error {
    s.port = port
    return nil
}
";
        let analyzer = GoAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Server.Start");
        assert_eq!(functions[0].start_line, 6);
        assert_eq!(functions[0].end_line, 9);
    }

    #[test]
    fn extracts_simple_calls() {
        let source = br#"package main

func main() {
    x := foo()
    y := bar(x)
    baz(x, y)
}
"#;
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["foo", "bar", "baz"]);
        assert!(calls.iter().all(|c| !c.is_method_call));
    }

    #[test]
    fn extracts_package_qualified_calls() {
        let source = br#"package main

func main() {
    fmt.Println("hello")
    os.Exit(1)
}
"#;
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"fmt.Println"));
        assert!(callees.contains(&"os.Exit"));
        assert!(calls.iter().all(|c| c.is_method_call));
        assert_eq!(calls[0].receiver.as_deref(), Some("fmt"));
    }

    #[test]
    fn extracts_method_calls_on_receiver() {
        let source = br#"package main

func process(s *Server) {
    s.Start()
    s.HandleRequest()
}
"#;
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["s.Start", "s.HandleRequest"]);
        assert!(calls.iter().all(|c| c.is_method_call));
    }

    #[test]
    fn extracts_builtin_calls() {
        let source = br#"package main

func example() {
    s := make([]int, 10)
    n := len(s)
}
"#;
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"make"));
        assert!(callees.contains(&"len"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"package main\n";
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    // Kill extract_calls line-offset mutants (+ with - or *). Calls on lines 4, 5, 6
    // distinguish `row + 1` from `row * 1` and `row - 1`.
    #[test]
    fn it_reports_call_sites_on_correct_lines() {
        let source = b"package main

func main() {
    foo()
    bar()
    baz()
}
";
        let analyzer = GoAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "foo");
        assert_eq!(calls[0].line, 4);
        assert_eq!(calls[1].callee, "bar");
        assert_eq!(calls[1].line, 5);
        assert_eq!(calls[2].callee, "baz");
        assert_eq!(calls[2].line, 6);
    }
}
