use super::{CallSite, Function, LanguageAnalyzer, sha256_hex};
use tree_sitter::Parser;
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_kotlin() -> *const ();
}

/// The tree-sitter [`LanguageFn`] for Kotlin.
///
/// Compiled from vendored tree-sitter-kotlin grammar sources via build.rs.
const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_kotlin) };

pub struct KotlinAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&LANGUAGE.into())
        .expect("Error loading Kotlin grammar");
    parser
}

fn signature_text(source: &[u8], node: &tree_sitter::Node) -> String {
    let start = node.start_byte();
    let body = node.child_by_field_name("body");
    // Kotlin grammar doesn't use field names for function_body,
    // so we look for the function_body child by kind.
    let body_start = body
        .or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .find(|c| c.kind() == "function_body")
        })
        .map(|b| b.start_byte());
    let end = body_start.unwrap_or(node.end_byte());
    let raw = &source[start..end];
    String::from_utf8_lossy(raw).trim().to_string()
}

/// Extract the function name from a `function_declaration` node.
/// For extension functions like `fun String.isBlank(): Boolean`, returns `"String.isBlank"`.
/// For regular functions like `fun add(a: Int, b: Int): Int`, returns `"add"`.
fn function_name(source: &[u8], node: &tree_sitter::Node) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();

    // Look for simple_identifier (the function name) and a preceding user_type (extension receiver).
    let mut receiver: Option<String> = None;
    let mut name: Option<String> = None;

    for child in &children {
        if child.kind() == "user_type" && name.is_none() {
            // This is an extension receiver type (appears before the function name)
            receiver = child.utf8_text(source).ok().map(|s| s.to_string());
        }
        if child.kind() == "simple_identifier" {
            name = child.utf8_text(source).ok().map(|s| s.to_string());
            break;
        }
    }

    match (receiver, name) {
        (Some(recv), Some(n)) => Some(format!("{recv}.{n}")),
        (None, Some(n)) => Some(n),
        _ => None,
    }
}

fn extract_methods_from_body(
    source: &[u8],
    body_node: &tree_sitter::Node,
    qualifier: &str,
    functions: &mut Vec<Function>,
) {
    let mut cursor = body_node.walk();
    for child in body_node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(fname) = function_name(source, &child) {
                    let qualified = format!("{qualifier}.{fname}");
                    let signature = signature_text(source, &child);
                    let body_hash = {
                        let body_node = child
                            .child_by_field_name("body")
                            .or_else(|| {
                                let mut bc = child.walk();
                                child
                                    .children(&mut bc)
                                    .find(|c| c.kind() == "function_body")
                            })
                            .unwrap_or(child);
                        sha256_hex(&source[body_node.start_byte()..body_node.end_byte()])
                    };
                    functions.push(Function {
                        name: qualified,
                        signature,
                        start_line: child.start_position().row + 1,
                        end_line: child.end_position().row + 1,
                        body_hash,
                    });
                }
            }
            "secondary_constructor" => {
                let signature = {
                    let start = child.start_byte();
                    // Find the block (statements) child to exclude from signature
                    let mut sc = child.walk();
                    let block_start = child
                        .children(&mut sc)
                        .find(|c| c.kind() == "statements" || c.kind() == "{")
                        .map(|b| b.start_byte());
                    let end = block_start.unwrap_or(child.end_byte());
                    String::from_utf8_lossy(&source[start..end])
                        .trim()
                        .to_string()
                };
                let body_hash = {
                    let mut sc2 = child.walk();
                    let body_node = child
                        .children(&mut sc2)
                        .find(|c| c.kind() == "statements" || c.kind() == "{")
                        .unwrap_or(child);
                    sha256_hex(&source[body_node.start_byte()..body_node.end_byte()])
                };
                let name = format!("{qualifier}.constructor");
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

fn extract_from_class(
    source: &[u8],
    class_node: &tree_sitter::Node,
    functions: &mut Vec<Function>,
) {
    let class_name = {
        let mut cursor = class_node.walk();
        class_node
            .children(&mut cursor)
            .find(|c| c.kind() == "type_identifier")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("")
    };

    let body = {
        let mut cursor = class_node.walk();
        class_node
            .children(&mut cursor)
            .find(|c| c.kind() == "class_body" || c.kind() == "enum_class_body")
    };

    if let Some(body) = body {
        extract_methods_from_body(source, &body, class_name, functions);
        // Handle companion objects inside class
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "companion_object" {
                extract_from_companion_object(source, &child, class_name, functions);
            }
        }
    }
}

fn extract_from_object(source: &[u8], obj_node: &tree_sitter::Node, functions: &mut Vec<Function>) {
    let obj_name = {
        let mut cursor = obj_node.walk();
        obj_node
            .children(&mut cursor)
            .find(|c| c.kind() == "type_identifier")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("")
    };

    let body = {
        let mut cursor = obj_node.walk();
        obj_node
            .children(&mut cursor)
            .find(|c| c.kind() == "class_body")
    };

    if let Some(body) = body {
        extract_methods_from_body(source, &body, obj_name, functions);
    }
}

fn extract_from_companion_object(
    source: &[u8],
    comp_node: &tree_sitter::Node,
    class_name: &str,
    functions: &mut Vec<Function>,
) {
    let body = {
        let mut cursor = comp_node.walk();
        comp_node
            .children(&mut cursor)
            .find(|c| c.kind() == "class_body")
    };

    if let Some(body) = body {
        // Qualify companion object methods as ClassName.Companion.methodName
        let qualifier = format!("{class_name}.Companion");
        extract_methods_from_body(source, &body, &qualifier, functions);
    }
}

impl LanguageAnalyzer for KotlinAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Kotlin source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_declaration" => {
                    if let Some(name) = function_name(source, &child) {
                        let signature = signature_text(source, &child);
                        let body_hash = {
                            let body_node = child
                                .child_by_field_name("body")
                                .or_else(|| {
                                    let mut bc = child.walk();
                                    child
                                        .children(&mut bc)
                                        .find(|c| c.kind() == "function_body")
                                })
                                .unwrap_or(child);
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
                }
                "class_declaration" => {
                    extract_from_class(source, &child, &mut functions);
                }
                "object_declaration" => {
                    extract_from_object(source, &child, &mut functions);
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Kotlin source"))?;

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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Kotlin source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "import_list" {
                let mut list_cursor = child.walk();
                for header in child.children(&mut list_cursor) {
                    if header.kind() == "import_header" {
                        let text = header.utf8_text(source).unwrap_or("");
                        let import_path =
                            text.trim().trim_start_matches("import").trim().to_string();
                        if !import_path.is_empty() {
                            imports.push(import_path);
                        }
                    }
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
        let source = br#"fun add(a: Int, b: Int): Int {
    return a + b
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].signature, "fun add(a: Int, b: Int): Int");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_class_method() {
        let source = br#"class Calculator {
    fun add(a: Int, b: Int): Int {
        return a + b
    }
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.add");
        assert_eq!(functions[0].signature, "fun add(a: Int, b: Int): Int");
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_multiple_class_methods() {
        let source = br#"class Math {
    fun add(a: Int, b: Int): Int {
        return a + b
    }

    fun subtract(a: Int, b: Int): Int {
        return a - b
    }
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Math.add");
        assert_eq!(functions[1].name, "Math.subtract");
        assert_eq!(functions[1].signature, "fun subtract(a: Int, b: Int): Int");
    }

    #[test]
    fn extracts_extension_function() {
        let source = br#"fun String.isPalindrome(): Boolean {
    return this == this.reversed()
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "String.isPalindrome");
        assert_eq!(functions[0].signature, "fun String.isPalindrome(): Boolean");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_extension_function_with_params() {
        let source = br#"fun List<Int>.sumWith(other: List<Int>): List<Int> {
    return this.zip(other).map { (a, b) -> a + b }
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "List<Int>.sumWith");
    }

    #[test]
    fn extracts_imports() {
        let source = br#"import kotlin.collections.List
import kotlin.io.println

fun main() {}
"#;
        let analyzer = KotlinAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec!["kotlin.collections.List", "kotlin.io.println"]
        );
    }

    #[test]
    fn extracts_wildcard_import() {
        let source = br#"import kotlin.collections.*

fun main() {}
"#;
        let analyzer = KotlinAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["kotlin.collections.*"]);
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"fun main() {
    println("Hello")
}
"#;
        let analyzer = KotlinAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn line_numbers_are_accurate_for_multiline() {
        let source = br#"package com.example

import kotlin.math.sqrt

fun distance(x1: Double, y1: Double, x2: Double, y2: Double): Double {
    val dx = x2 - x1
    val dy = y2 - y1
    return sqrt(dx * dx + dy * dy)
}

fun midpoint(x1: Double, y1: Double, x2: Double, y2: Double): Pair<Double, Double> {
    return Pair((x1 + x2) / 2, (y1 + y2) / 2)
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "distance");
        assert_eq!(functions[0].start_line, 5);
        assert_eq!(functions[0].end_line, 9);
        assert_eq!(functions[1].name, "midpoint");
        assert_eq!(functions[1].start_line, 11);
        assert_eq!(functions[1].end_line, 13);
    }

    #[test]
    fn extracts_object_method() {
        let source = br#"object Singleton {
    fun instance(): Singleton {
        return this
    }
}
"#;
        let analyzer = KotlinAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Singleton.instance");
    }

    #[test]
    fn extracts_function_calls() {
        let source = br#"fun process() {
    val x = calculate(42)
    println(x)
    obj.doWork()
}
"#;
        let analyzer = KotlinAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"println"));
        assert!(callees.contains(&"obj.doWork"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = KotlinAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }
}
