use super::{CallSite, Function, LanguageAnalyzer, body_hash_for_node};
use tree_sitter::Parser;

pub struct SwiftAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_swift::LANGUAGE.into())
        .expect("Error loading Swift grammar");
    parser
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
    let start = node.start_byte();
    let body = node.child_by_field_name("body");
    let end = body.map_or(node.end_byte(), |b| b.start_byte());
    let raw = &source[start..end];
    String::from_utf8_lossy(raw).trim().to_string()
}

fn extract_methods_from_class(
    source: &[u8],
    class_node: &tree_sitter::Node,
    functions: &mut Vec<Function>,
) {
    let class_name = class_node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");

    let body = match class_node.child_by_field_name("body") {
        Some(b) => b,
        None => return,
    };

    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                let method_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let name = format!("{class_name}.{method_name}");
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
            "init_declaration" => {
                let name = format!("{class_name}.init");
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
}

impl LanguageAnalyzer for SwiftAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Swift source"))?;
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
                "class_declaration" => {
                    extract_methods_from_class(source, &child, &mut functions);
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Swift source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression"
                && let Some(func) = node.child(0)
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match func.kind() {
                    "navigation_expression" => {
                        let recv = func
                            .child(0)
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Swift source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "import_declaration" {
                // Strip "import " prefix and trim
                let text = child.utf8_text(source).unwrap_or("");
                let import_path = text.trim().trim_start_matches("import").trim().to_string();
                if !import_path.is_empty() {
                    imports.push(import_path);
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
    fn extracts_standalone_function() {
        let source = br#"func greet(name: String) -> String {
    return "Hello, \(name)!"
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "greet");
        assert_eq!(functions[0].signature, "func greet(name: String) -> String");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_class_method() {
        let source = br#"class Calculator {
    func add(a: Int, b: Int) -> Int {
        return a + b
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.add");
        assert_eq!(functions[0].signature, "func add(a: Int, b: Int) -> Int");
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_init_declaration() {
        let source = br#"class Person {
    init(name: String) {
        self.name = name
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Person.init");
        assert_eq!(functions[0].signature, "init(name: String)");
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_struct_method() {
        let source = br#"struct Point {
    func distance(to other: Point) -> Double {
        return 0.0
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Point.distance");
        assert_eq!(
            functions[0].signature,
            "func distance(to other: Point) -> Double"
        );
    }

    #[test]
    fn extracts_multiple_class_methods() {
        let source = br#"class Math {
    func add(a: Int, b: Int) -> Int {
        return a + b
    }

    func subtract(a: Int, b: Int) -> Int {
        return a - b
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Math.add");
        assert_eq!(functions[1].name, "Math.subtract");
        assert_eq!(
            functions[1].signature,
            "func subtract(a: Int, b: Int) -> Int"
        );
    }

    #[test]
    fn extracts_class_with_init_and_methods() {
        let source = br#"class Person {
    private var name: String

    init(name: String) {
        self.name = name
    }

    func getName() -> String {
        return self.name
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Person.init");
        assert_eq!(functions[0].signature, "init(name: String)");
        assert_eq!(functions[1].name, "Person.getName");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_single_import() {
        let source = br#"import Foundation

func foo() {}
"#;
        let analyzer = SwiftAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["Foundation"]);
    }

    #[test]
    fn extracts_multiple_imports() {
        let source = br#"import Foundation
import UIKit
import SwiftUI

class Foo {}
"#;
        let analyzer = SwiftAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["Foundation", "UIKit", "SwiftUI"]);
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"func foo() {
    print("hello")
}
"#;
        let analyzer = SwiftAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn line_numbers_are_accurate_for_nested_method() {
        let source = br#"import Foundation

class Service {
    func start() {
        print("starting")
    }

    func stop() {
        print("stopping")
    }
}
"#;
        let analyzer = SwiftAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Service.start");
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 6);
        assert_eq!(functions[1].name, "Service.stop");
        assert_eq!(functions[1].start_line, 8);
        assert_eq!(functions[1].end_line, 10);
    }

    #[test]
    fn extracts_function_calls() {
        let source = br#"func process() {
    let x = calculate(input: 42)
    print(x)
    obj.doWork()
}
"#;
        let analyzer = SwiftAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"print"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = SwiftAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    // Kill "delete match arm navigation_expression" mutant: assert that a
    // method call expressed as `obj.method()` is flagged is_method_call=true
    // with receiver populated. Without the navigation_expression arm, both
    // would fall through to (false, None).
    #[test]
    fn it_reports_method_call_with_receiver_for_navigation_expression() {
        let source = br#"func process() {
    obj.doWork()
}
"#;
        let analyzer = SwiftAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let do_work = calls
            .iter()
            .find(|c| c.callee == "obj.doWork")
            .expect("obj.doWork call must be present");
        assert!(
            do_work.is_method_call,
            "navigation_expression call must be flagged as method call"
        );
        assert_eq!(do_work.receiver.as_deref(), Some("obj"));
    }

    // Kill extract_calls line-offset mutants (+ with - or *). Calls on lines 2, 3, 4
    // distinguish `row + 1` from `row * 1` and `row - 1`.
    #[test]
    fn it_reports_call_sites_on_correct_lines() {
        let source = b"func run() {
    foo()
    bar()
    baz()
}
";
        let analyzer = SwiftAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let foo = calls.iter().find(|c| c.callee == "foo").expect("foo call");
        let bar = calls.iter().find(|c| c.callee == "bar").expect("bar call");
        let baz = calls.iter().find(|c| c.callee == "baz").expect("baz call");
        assert_eq!(foo.line, 2);
        assert_eq!(bar.line, 3);
        assert_eq!(baz.line, 4);
    }
}
