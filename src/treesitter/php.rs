use super::{CallSite, Function, LanguageAnalyzer, body_hash_for_node};
use tree_sitter::Parser;

pub struct PhpAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
        .expect("Error loading PHP grammar");
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
        if child.kind() == "method_declaration" {
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
    }
}

impl LanguageAnalyzer for PhpAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse PHP source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();

        // PHP files are wrapped in a `program` node. Walk its children to find
        // top-level function definitions and class declarations.
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_definition" => {
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse PHP source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "function_call_expression" => {
                    if let Some(func) = node.child_by_field_name("function") {
                        let callee = func.utf8_text(source).unwrap_or("").to_string();
                        calls.push(CallSite {
                            callee,
                            line: node.start_position().row + 1,
                            is_method_call: false,
                            receiver: None,
                        });
                    }
                }
                "member_call_expression" => {
                    let name = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("");
                    let recv = node
                        .child_by_field_name("object")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|s| s.to_string());
                    let callee = match &recv {
                        Some(r) => format!("{r}.{name}"),
                        None => name.to_string(),
                    };
                    calls.push(CallSite {
                        callee,
                        line: node.start_position().row + 1,
                        is_method_call: true,
                        receiver: recv,
                    });
                }
                _ => {}
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse PHP source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "namespace_use_declaration" {
                // Strip "use" keyword and trailing ";", keep only the namespace path.
                let text = child.utf8_text(source).unwrap_or("");
                let import_path = text
                    .trim()
                    .trim_start_matches("use")
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_string();
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
        let source = br#"<?php
function greet(string $name): string {
    return "Hello, $name!";
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "greet");
        assert_eq!(
            functions[0].signature,
            "function greet(string $name): string"
        );
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_class_method() {
        let source = br#"<?php
class Calculator {
    public function add(int $a, int $b): int {
        return $a + $b;
    }
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.add");
        assert_eq!(
            functions[0].signature,
            "public function add(int $a, int $b): int"
        );
        assert_eq!(functions[0].start_line, 3);
        assert_eq!(functions[0].end_line, 5);
    }

    #[test]
    fn extracts_multiple_class_methods() {
        let source = br#"<?php
class Math {
    public function add(int $a, int $b): int {
        return $a + $b;
    }

    public function subtract(int $a, int $b): int {
        return $a - $b;
    }
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Math.add");
        assert_eq!(functions[1].name, "Math.subtract");
        assert_eq!(
            functions[1].signature,
            "public function subtract(int $a, int $b): int"
        );
    }

    #[test]
    fn extracts_constructor() {
        let source = br#"<?php
class Person {
    private string $name;

    public function __construct(string $name) {
        $this->name = $name;
    }

    public function getName(): string {
        return $this->name;
    }
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Person.__construct");
        assert_eq!(
            functions[0].signature,
            "public function __construct(string $name)"
        );
        assert_eq!(functions[1].name, "Person.getName");
    }

    #[test]
    fn extracts_static_method() {
        let source = br#"<?php
class Utils {
    public static function format(string $s): string {
        return trim($s);
    }
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Utils.format");
        assert_eq!(
            functions[0].signature,
            "public static function format(string $s): string"
        );
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"<?php\n";
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_single_use_import() {
        let source = br#"<?php
use App\Models\User;

class Foo {}
"#;
        let analyzer = PhpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec![r"App\Models\User"]);
    }

    #[test]
    fn extracts_multiple_use_imports() {
        let source = br#"<?php
use App\Models\User;
use App\Models\Post;
use Illuminate\Support\Facades\Log;

class Foo {}
"#;
        let analyzer = PhpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec![
                r"App\Models\User",
                r"App\Models\Post",
                r"Illuminate\Support\Facades\Log"
            ]
        );
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"<?php
function foo() {}
"#;
        let analyzer = PhpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn line_numbers_are_accurate_for_standalone_function() {
        let source = br#"<?php

// Some comment

function doStuff(int $x): void {
    echo $x;
    echo $x * 2;
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].start_line, 5);
        assert_eq!(functions[0].end_line, 8);
    }

    #[test]
    fn line_numbers_are_accurate_for_class_methods() {
        let source = br#"<?php

class Service {
    public function handle(): void {
        // line 5
    }

    public function process(): void {
        // line 9
    }
}
"#;
        let analyzer = PhpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Service.handle");
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 6);
        assert_eq!(functions[1].name, "Service.process");
        assert_eq!(functions[1].start_line, 8);
        assert_eq!(functions[1].end_line, 10);
    }

    #[test]
    fn extracts_function_and_method_calls() {
        let source = br#"<?php
function process() {
    $x = calculate($input);
    $obj->doWork();
    array_map(fn($v) => $v * 2, $arr);
}
"#;
        let analyzer = PhpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"$obj.doWork"));
        assert!(callees.contains(&"array_map"));
    }

    #[test]
    fn empty_php_returns_no_calls() {
        let source = b"<?php\n";
        let analyzer = PhpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }
}
