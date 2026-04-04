use super::{Function, LanguageAnalyzer};
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
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                // Handle pointer receivers like *Server
                let type_text = type_node.utf8_text(source).unwrap_or("");
                return type_text.trim_start_matches('*').to_string();
            }
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
                    if spec.kind() == "import_spec" {
                        if let Some(path) = import_path_from_spec(source, &spec) {
                            imports.push(path);
                        }
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
        let tree = parser.parse(source, None).ok_or_else(|| anyhow::anyhow!("Failed to parse Go source"))?;
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
                    functions.push(Function {
                        name,
                        signature,
                        start_line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
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
                    functions.push(Function {
                        name,
                        signature,
                        start_line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
                    });
                }
                _ => {}
            }
        }

        Ok(functions)
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
}
