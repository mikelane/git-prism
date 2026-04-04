use super::{Function, LanguageAnalyzer};
use tree_sitter::Parser;

pub struct PythonAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .expect("Error loading Python grammar");
    parser
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
    // For Python, signature is from start of node to the colon before the body
    let start = node.start_byte();
    let body = node.child_by_field_name("body");
    let end = body.map_or(node.end_byte(), |b| b.start_byte());
    let raw = &source[start..end];
    String::from_utf8_lossy(raw).trim().to_string()
}

fn extract_functions_from_node(
    source: &[u8],
    node: &tree_sitter::Node,
    class_name: Option<&str>,
    functions: &mut Vec<Function>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                let fn_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let name = match class_name {
                    Some(cls) => format!("{cls}.{fn_name}"),
                    None => fn_name.to_string(),
                };
                let signature = signature_text(source, &child);
                functions.push(Function {
                    name,
                    signature,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                });
            }
            "class_definition" => {
                let cls_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                functions.push(Function {
                    name: cls_name.to_string(),
                    signature: signature_text(source, &child),
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                });
                if let Some(body) = child.child_by_field_name("body") {
                    extract_functions_from_node(source, &body, Some(cls_name), functions);
                }
            }
            _ => {}
        }
    }
}

impl LanguageAnalyzer for PythonAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();
        extract_functions_from_node(source, &root, None, &mut functions);
        Ok(functions)
    }

    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "import_statement" | "import_from_statement" => {
                    let text = child.utf8_text(source).unwrap_or("").trim().to_string();
                    imports.push(text);
                }
                _ => {}
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
        let source = br#"def hello():
    print("hello")
"#;
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "hello");
        assert_eq!(functions[0].signature, "def hello():");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 2);
    }

    #[test]
    fn extracts_class_and_methods() {
        let source = br#"class MyClass:
    def __init__(self):
        pass

    def do_thing(self, x: int) -> str:
        return str(x)
"#;
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 3);
        assert_eq!(functions[0].name, "MyClass");
        assert_eq!(functions[1].name, "MyClass.__init__");
        assert_eq!(functions[1].signature, "def __init__(self):");
        assert_eq!(functions[2].name, "MyClass.do_thing");
        assert_eq!(functions[2].signature, "def do_thing(self, x: int) -> str:");
    }

    #[test]
    fn extracts_function_with_params_and_return_type() {
        let source = br#"def add(a: int, b: int) -> int:
    return a + b
"#;
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].signature, "def add(a: int, b: int) -> int:");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_import_statement() {
        let source = br#"import os
import sys
"#;
        let analyzer = PythonAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["import os", "import sys"]);
    }

    #[test]
    fn extracts_from_import_statement() {
        let source = br#"from os.path import join
from typing import List, Optional
"#;
        let analyzer = PythonAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec!["from os.path import join", "from typing import List, Optional"]
        );
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"def hello():
    pass
"#;
        let analyzer = PythonAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }
}
