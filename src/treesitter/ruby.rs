use super::{body_hash_for_node, CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH};
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
                    extract_functions_from_node(
                        source,
                        &body,
                        Some(cls_name),
                        functions,
                        depth + 1,
                    );
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
        extract_functions_from_node(source, &root, None, &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Ruby source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call" {
                let method = node
                    .child_by_field_name("method")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let (callee, is_method_call, receiver) =
                    if let Some(recv_node) = node.child_by_field_name("receiver") {
                        let recv_text = recv_node.utf8_text(source).unwrap_or("").to_string();
                        (format!("{recv_text}.{method}"), true, Some(recv_text))
                    } else {
                        (method.to_string(), false, None)
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
    use tracing_test::traced_test;

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

    #[test]
    fn extracts_function_and_method_calls() {
        let source = br#"def process
  x = calculate(input)
  puts x
  obj.do_work
  result = transform(x)
end
"#;
        let analyzer = RubyAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"puts"));
        assert!(callees.contains(&"obj.do_work"));
        assert!(callees.contains(&"transform"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = RubyAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Security regression: deeply-nested class declarations used to stack-overflow
    /// `extract_functions_from_node` because it recursed into each class body without
    /// a depth limit. An attacker committing a Ruby file with thousands of nested
    /// Depth-guard warning: when `extract_functions_from_node` hits MAX_RECURSION_DEPTH
    /// it must emit a tracing::warn! so operators can observe truncation in logs/OTLP.
    ///
    /// Uses 300 nesting levels — past MAX_RECURSION_DEPTH (256) but shallow enough
    /// to run on the default test stack without spawning a new thread.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_classes() {
        const NESTING_DEPTH: usize = 300;

        let mut source = String::new();
        for i in 0..NESTING_DEPTH {
            source.push_str(&format!("class C{i}\n"));
        }
        for _ in 0..NESTING_DEPTH {
            source.push_str("end\n");
        }

        let analyzer = RubyAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_input() {
        let source = b"class Foo\n  def bar\n  end\nend\n";
        let analyzer = RubyAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// class blocks could crash git-prism during `get_change_manifest`. The analyzer
    /// must now complete without crashing.
    ///
    /// Runs on a thread with a bounded 512KB stack to force the crash to be
    /// reproducible regardless of the host's default stack size.
    #[test]
    fn deeply_nested_classes_do_not_stack_overflow() {
        const NESTING_DEPTH: usize = 5000;
        // 2 MB: roomy enough for bounded recursion to `MAX_RECURSION_DEPTH`
        // but far too small for unbounded recursion to 5000 frames.
        const TEST_STACK_SIZE: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..NESTING_DEPTH {
            source.push_str(&format!("class C{i}\n"));
        }
        for _ in 0..NESTING_DEPTH {
            source.push_str("end\n");
        }

        let handle = std::thread::Builder::new()
            .stack_size(TEST_STACK_SIZE)
            .spawn(move || {
                let analyzer = RubyAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        let functions = result.expect("analyzer must return Ok on deeply-nested input");
        // At least the outermost classes (up to MAX_RECURSION_DEPTH) must be
        // returned — the depth guard truncates deeper nesting but preserves
        // whatever extraction completed successfully.
        assert!(
            !functions.is_empty(),
            "expected partial extraction to include outer classes"
        );
    }
}
