use super::{CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH, body_hash_for_node};
use tree_sitter::Parser;

#[derive(Debug, Clone, Copy)]
enum JsDialect {
    TypeScript,
    Tsx,
    JavaScript,
}

pub struct TypeScriptAnalyzer {
    dialect: JsDialect,
}

impl TypeScriptAnalyzer {
    pub fn typescript() -> Self {
        Self {
            dialect: JsDialect::TypeScript,
        }
    }

    pub fn tsx() -> Self {
        Self {
            dialect: JsDialect::Tsx,
        }
    }

    pub fn javascript() -> Self {
        Self {
            dialect: JsDialect::JavaScript,
        }
    }

    fn create_parser(&self) -> Parser {
        let mut parser = Parser::new();
        let language = match self.dialect {
            JsDialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            JsDialect::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            JsDialect::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        };
        parser
            .set_language(&language)
            .expect("Error loading grammar");
        parser
    }
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
            "method_definition" => {
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
            "class_declaration" => {
                let cls_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
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
            "lexical_declaration" => {
                // Handle named arrow functions: const foo = () => {}
                let mut decl_cursor = child.walk();
                for decl_child in child.children(&mut decl_cursor) {
                    if decl_child.kind() == "variable_declarator" {
                        let value = decl_child.child_by_field_name("value");
                        let is_arrow = value.map(|v| v.kind() == "arrow_function").unwrap_or(false);
                        if is_arrow {
                            let fn_name = decl_child
                                .child_by_field_name("name")
                                .and_then(|n| n.utf8_text(source).ok())
                                .unwrap_or("");
                            let arrow_node = value.unwrap();
                            let signature = signature_text(source, &child);
                            let body_hash = body_hash_for_node(source, arrow_node);
                            functions.push(Function {
                                name: fn_name.to_string(),
                                signature,
                                start_line: child.start_position().row + 1,
                                end_line: arrow_node.end_position().row + 1,
                                body_hash,
                            });
                        }
                    }
                }
            }
            "export_statement" => {
                extract_functions_from_node(source, &child, class_name, functions, depth + 1);
            }
            _ => {}
        }
    }
}

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = self.create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();
        extract_functions_from_node(source, &root, None, &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = self.create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call_expression"
                && let Some(func) = node.child_by_field_name("function")
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match func.kind() {
                    "member_expression" => {
                        let recv = func
                            .child_by_field_name("object")
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
        let mut parser = self.create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "import_statement" {
                let text = child.utf8_text(source).unwrap_or("").trim().to_string();
                imports.push(text);
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
    fn extracts_simple_function() {
        let source = br#"function greet(name: string): void {
    console.log(name);
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "greet");
        assert_eq!(functions[0].signature, "function greet(name: string): void");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_class_methods() {
        let source = br#"class Greeter {
    greet(name: string): void {
        console.log(name);
    }

    farewell(): void {
        console.log("bye");
    }
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Greeter.greet");
        assert_eq!(functions[0].signature, "greet(name: string): void");
        assert_eq!(functions[1].name, "Greeter.farewell");
    }

    #[test]
    fn extracts_named_arrow_function() {
        let source = br#"const add = (a: number, b: number): number => {
    return a + b;
};
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "add");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_es_imports() {
        let source = br#"import { foo, bar } from './utils';
import * as path from 'path';
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0], "import { foo, bar } from './utils';");
        assert_eq!(imports[1], "import * as path from 'path';");
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"function hello() {
    return 1;
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn handles_javascript_syntax() {
        let source = br#"function greet(name) {
    console.log(name);
}
"#;
        let analyzer = TypeScriptAnalyzer::javascript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "greet");
        assert_eq!(functions[0].signature, "function greet(name)");
    }

    #[test]
    fn javascript_dialect_parses_commonjs_require() {
        let source = br#"const fs = require('fs');
const path = require('path');

function readFile(name) {
    return fs.readFileSync(name);
}
"#;
        let analyzer = TypeScriptAnalyzer::javascript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "readFile");
    }

    #[test]
    fn tsx_dialect_parses_jsx_component() {
        let source = br#"import React from 'react';

function App(): JSX.Element {
    return <div>Hello</div>;
}
"#;
        let analyzer = TypeScriptAnalyzer::tsx();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "App");
    }

    #[test]
    fn javascript_dialect_extracts_function_expression() {
        let source = br#"var greet = function(name) {
    console.log(name);
};
"#;
        let analyzer = TypeScriptAnalyzer::javascript();
        let functions = analyzer.extract_functions(source).unwrap();
        // var declarations with function expressions are not named arrow functions,
        // so they won't be extracted (consistent with current behavior)
        assert!(functions.is_empty());
    }

    #[test]
    fn tsx_dialect_extracts_imports() {
        let source = br#"import React from 'react';
import { useState } from 'react';
"#;
        let analyzer = TypeScriptAnalyzer::tsx();
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0], "import React from 'react';");
        assert_eq!(imports[1], "import { useState } from 'react';");
    }

    #[test]
    fn typescript_dialect_parses_typed_code() {
        let source = br#"function add(a: number, b: number): number {
    return a + b;
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "add");
        assert_eq!(
            functions[0].signature,
            "function add(a: number, b: number): number"
        );
    }

    // Kill mutants: replace + with * or - in method_definition line number arithmetic (row + 1).
    // Method at row > 0 ensures row+1 != row*1 and row+1 != row-1.
    #[test]
    fn it_reports_correct_line_numbers_for_method_definition() {
        let source = b"// comment line 1
// comment line 2

class Greeter {
    greet(name: string): void {
        console.log(name);
    }
}
";
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Greeter.greet");
        assert_eq!(functions[0].start_line, 5);
        assert_eq!(functions[0].end_line, 7);
    }

    #[test]
    fn extracts_simple_calls() {
        let source = br#"function main() {
    const x = foo();
    const y = bar(x);
    baz(x, y);
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["foo", "bar", "baz"]);
        assert!(calls.iter().all(|c| !c.is_method_call));
    }

    #[test]
    fn extracts_method_calls() {
        let source = br#"function process(server: Server) {
    server.start();
    server.handleRequest();
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["server.start", "server.handleRequest"]);
        assert!(calls.iter().all(|c| c.is_method_call));
        assert_eq!(calls[0].receiver.as_deref(), Some("server"));
    }

    #[test]
    fn extracts_console_log() {
        let source = br#"function example() {
    console.log("hello");
    Array.from([1, 2]);
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"console.log"));
        assert!(callees.contains(&"Array.from"));
    }

    #[test]
    fn extracts_calls_inside_callbacks() {
        let source = br#"function example() {
    setTimeout(() => {
        doWork();
    }, 1000);
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"setTimeout"));
        assert!(callees.contains(&"doWork"));
    }

    #[test]
    fn javascript_extracts_calls() {
        let source = br#"function main() {
    const result = calculate(input);
    console.log(result);
}
"#;
        let analyzer = TypeScriptAnalyzer::javascript();
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"console.log"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = TypeScriptAnalyzer::typescript();
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Depth-guard warning: the `tracing::warn!` in `extract_functions_from_node` is
    /// defense-in-depth for future grammar changes. With the current tree-sitter TypeScript
    /// grammar, the guard is not reachable via any syntactically meaningful input:
    ///
    /// - `class C0 { class C1 { ... } }` → tree-sitter error-recovers inner classes to
    ///   ERROR nodes; the `class_declaration` arm is never matched inside a class body.
    /// - `export export export class C {}` → extra `export` keywords become a single
    ///   ERROR node; nested `export_statement` nodes are never produced.
    /// - TypeScript namespaces parse as `internal_module > statement_block`; the `_` arm
    ///   does not recurse, so namespace depth never increments the recursion counter.
    ///
    /// The warn! is present for when a future grammar version produces such nesting.
    /// The triangulation test below confirms shallow input never emits the warning.
    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_input() {
        let source = b"export class Foo { bar(): void {} }\n";
        let analyzer = TypeScriptAnalyzer::typescript();
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Defense-in-depth: deeply-nested TypeScript export_statement and class_declaration
    /// bodies are guarded by `MAX_RECURSION_DEPTH`. The two recursion sites in
    /// `extract_functions_from_node` are `class_declaration` body and `export_statement`.
    ///
    /// Investigation per the plan: valid TypeScript syntax does not allow
    /// `class A { class B {} }` at the declaration level, and the tree-sitter
    /// TypeScript grammar's error recovery produces ERROR nodes (not nested
    /// `class_declaration` nodes) for malformed input. Similarly, stacked `export`
    /// keywords are parse errors and error-recover to a single `export_statement`
    /// wrapping the inner tokens, not to recursively nested `export_statement` nodes.
    /// Because the walker's match is explicit (`"export_statement"` and `"class_declaration"`),
    /// ERROR-kind nodes are invisible to the recursion path.
    ///
    /// Guard added for defense-in-depth and consistency. This test verifies the
    /// walker completes without crashing on a deeply-nested (but grammar-limited)
    /// export chain, and that extraction still works correctly.
    #[test]
    fn it_completes_without_overflow_on_deeply_stacked_export_keywords() {
        const GENERATED_NESTING_LEVELS: usize = 5000;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        // Stacked `export` keywords — tree-sitter error-recovers these, so
        // they don't produce nested export_statement nodes in practice.
        // The test still validates the analyzer handles the input safely.
        let mut source = String::new();
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("export ");
        }
        source.push_str("class C {}\n");

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = TypeScriptAnalyzer::typescript();
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        result.expect("analyzer must return Ok on deeply-nested input");
        // No assertion on function count — the parse tree shape is grammar-dependent.
    }

    /// Triangulation: sequential exported classes with methods must all extract.
    /// This confirms the guard does not fire on legitimate export_statement usage.
    /// NOTE: These are sequential (not nested) exports — each recurses at depth 1,
    /// not depth 255. This tests non-regression of the export_statement arm, not
    /// the depth-boundary property (tree-sitter error-recovers stacked `export`
    /// keywords to a single export_statement, so true depth-255 nesting via exports
    /// is not achievable with valid or error-recovered TypeScript syntax).
    #[test]
    fn it_extracts_methods_from_exported_classes() {
        const CLASS_COUNT: usize = 255;

        // Build 255 sequential (not nested) exported classes each with a method,
        // to confirm the export_statement arm still works at high volume.
        let mut source = String::new();
        for i in 0..CLASS_COUNT {
            source.push_str(&format!("export class C{i} {{ method{i}(): void {{}} }}\n"));
        }

        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        assert_eq!(
            functions.len(),
            CLASS_COUNT,
            "all {CLASS_COUNT} methods must be extracted"
        );
    }

    #[test]
    fn extracts_export_function() {
        let source = br#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "greet");
    }

    #[test]
    fn extracts_export_default_function() {
        let source = br#"export default function handler(req: any): any {
    return { status: 200 };
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "handler");
    }

    #[test]
    fn extracts_export_class_methods() {
        let source = br#"export class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }
}
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.add");
    }

    #[test]
    fn extracts_export_const_arrow_function() {
        let source = br#"export const add = (a: number, b: number): number => {
    return a + b;
};
"#;
        let analyzer = TypeScriptAnalyzer::typescript();
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "add");
    }
}
