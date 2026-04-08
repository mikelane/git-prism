use super::{Function, LanguageAnalyzer, body_hash_for_node};
use tree_sitter::Parser;

pub struct RubyAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_ruby::LANGUAGE.into())
        .expect("Error loading Ruby grammar");
    parser
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
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
            "method" => {
                let method_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let name = match class_name {
                    Some(cls) => format!("{cls}.{method_name}"),
                    None => method_name.to_string(),
                };
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
            "singleton_method" => {
                let method_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let name = match class_name {
                    Some(cls) => format!("{cls}.{method_name}"),
                    None => format!("self.{method_name}"),
                };
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
            "class" => {
                let cls_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let body_hash = body_hash_for_node(source, child);
                functions.push(Function {
                    name: cls_name.to_string(),
                    signature: signature_text(source, &child),
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    body_hash,
                });
                if let Some(body) = child.child_by_field_name("body") {
                    extract_functions_from_node(source, &body, Some(cls_name), functions);
                }
            }
            _ => {}
        }
    }
}

impl LanguageAnalyzer for RubyAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Ruby source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();
        extract_functions_from_node(source, &root, None, &mut functions);
        Ok(functions)
    }

    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Ruby source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "call" {
                let method_name = child
                    .child_by_field_name("method")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                if method_name == "require" || method_name == "require_relative" {
                    let text = child.utf8_text(source).unwrap_or("").trim().to_string();
                    imports.push(text);
                }
            }
        }

        Ok(imports)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_standalone_method() {
        let source = br#"def hello
  puts "hello"
end
"#;
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "hello");
        assert_eq!(functions[0].signature, "def hello");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_standalone_method_with_params() {
        let source = br#"def add(a, b)
  a + b
end
"#;
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].signature, "def add(a, b)");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_class_and_methods() {
        let source = br#"class MyClass
  def initialize(name)
    @name = name
  end

  def greet
    puts "Hello"
  end
end
"#;
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 3);
        assert_eq!(functions[0].name, "MyClass");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 9);
        assert_eq!(functions[1].name, "MyClass.initialize");
        assert_eq!(functions[1].signature, "def initialize(name)");
        assert_eq!(functions[1].start_line, 2);
        assert_eq!(functions[1].end_line, 4);
        assert_eq!(functions[2].name, "MyClass.greet");
        assert_eq!(functions[2].signature, "def greet");
        assert_eq!(functions[2].start_line, 6);
        assert_eq!(functions[2].end_line, 8);
    }

    #[test]
    fn extracts_singleton_method() {
        let source = br#"class Factory
  def self.create(name)
    Factory.new(name)
  end
end
"#;
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Factory");
        assert_eq!(functions[1].name, "Factory.create");
        assert_eq!(functions[1].signature, "def self.create(name)");
        assert_eq!(functions[1].start_line, 2);
        assert_eq!(functions[1].end_line, 4);
    }

    #[test]
    fn extracts_require_imports() {
        let source = br#"require 'json'
require 'net/http'
"#;
        let analyzer = RubyAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["require 'json'", "require 'net/http'"]);
    }

    #[test]
    fn extracts_require_relative_imports() {
        let source = br#"require_relative 'helper'
require_relative 'lib/utils'
"#;
        let analyzer = RubyAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec!["require_relative 'helper'", "require_relative 'lib/utils'"]
        );
    }

    #[test]
    fn extracts_mixed_imports() {
        let source = br#"require 'json'
require_relative 'helper'
"#;
        let analyzer = RubyAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["require 'json'", "require_relative 'helper'"]);
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"def hello
  puts "hello"
end
"#;
        let analyzer = RubyAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    // Kill mutants: class at row > 0 ensures row+1 != row*1 and row+1 != row-1.
    #[test]
    fn it_reports_correct_line_numbers_for_class_definition() {
        let source = b"# comment line 1
# comment line 2

class MyClass
  def method
    nil
  end
end
";
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "MyClass");
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 8);
        assert_eq!(functions[1].name, "MyClass.method");
        assert_eq!(functions[1].start_line, 5);
        assert_eq!(functions[1].end_line, 7);
    }

    #[test]
    fn standalone_singleton_method_uses_self_prefix() {
        // A singleton method outside a class (unusual but valid Ruby)
        let source = br#"def self.standalone_class_method
  42
end
"#;
        let analyzer = RubyAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "self.standalone_class_method");
        assert_eq!(functions[0].signature, "def self.standalone_class_method");
    }
}
