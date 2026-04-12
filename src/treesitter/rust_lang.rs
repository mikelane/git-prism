use super::{body_hash_for_node, CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH};
use tree_sitter::Parser;

pub struct RustAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("Error loading Rust grammar");
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
    type_name: Option<&str>,
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
            "function_item" => {
                let fn_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let name = match type_name {
                    Some(t) => format!("{t}::{fn_name}"),
                    None => fn_name.to_string(),
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
            "impl_item" => {
                let impl_type = child
                    .child_by_field_name("type")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                if let Some(body) = child.child_by_field_name("body") {
                    extract_functions_from_node(
                        source,
                        &body,
                        Some(impl_type),
                        functions,
                        depth + 1,
                    );
                }
            }
            _ => {}
        }
    }
}

impl LanguageAnalyzer for RustAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();
        extract_functions_from_node(source, &root, None, &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            match node.kind() {
                "call_expression" => {
                    if let Some(func) = node.child_by_field_name("function") {
                        let callee = func.utf8_text(source).unwrap_or("").to_string();
                        let (is_method_call, receiver) = match func.kind() {
                            "field_expression" => {
                                let recv = func
                                    .child_by_field_name("value")
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
                }
                "macro_invocation" => {
                    if let Some(macro_node) = node.child_by_field_name("macro") {
                        let callee = format!("{}!", macro_node.utf8_text(source).unwrap_or(""));
                        calls.push(CallSite {
                            callee,
                            line: node.start_position().row + 1,
                            is_method_call: false,
                            receiver: None,
                        });
                    }
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "use_declaration" {
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
        let source = br#"fn hello() {
    println!("hello");
}
"#;
        let analyzer = RustAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "hello");
        assert_eq!(functions[0].signature, "fn hello()");
        assert_eq!(functions[0].start_line, 1);
        assert_eq!(functions[0].end_line, 3);
    }

    #[test]
    fn extracts_impl_methods() {
        let source = br#"struct Server {
    port: u16,
}

impl Server {
    fn new(port: u16) -> Self {
        Server { port }
    }

    fn start(&self) {
        println!("Starting on {}", self.port);
    }
}
"#;
        let analyzer = RustAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Server::new");
        assert_eq!(functions[0].signature, "fn new(port: u16) -> Self");
        assert_eq!(functions[1].name, "Server::start");
        assert_eq!(functions[1].signature, "fn start(&self)");
    }

    #[test]
    fn extracts_function_with_params_and_return() {
        let source = br#"fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
        let analyzer = RustAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].signature, "fn add(a: i32, b: i32) -> i32");
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = RustAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_use_declarations() {
        let source = br#"use std::io;
use std::collections::HashMap;
use anyhow::Result;
"#;
        let analyzer = RustAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0], "use std::io;");
        assert_eq!(imports[1], "use std::collections::HashMap;");
        assert_eq!(imports[2], "use anyhow::Result;");
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"fn hello() {
    println!("hello");
}
"#;
        let analyzer = RustAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn extracts_simple_calls() {
        let source = br#"fn main() {
    let x = foo();
    let y = bar(x);
    baz(x, y);
}
"#;
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["foo", "bar", "baz"]);
        assert!(calls.iter().all(|c| !c.is_method_call));
    }

    #[test]
    fn extracts_method_calls() {
        let source = br#"fn process(server: &Server) {
    server.start();
    server.handle_request();
}
"#;
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["server.start", "server.handle_request"]);
        assert!(calls.iter().all(|c| c.is_method_call));
        assert_eq!(calls[0].receiver.as_deref(), Some("server"));
    }

    #[test]
    fn extracts_scoped_calls() {
        let source = br#"fn example() {
    let map = std::collections::HashMap::new();
    Vec::with_capacity(10);
}
"#;
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"std::collections::HashMap::new"));
        assert!(callees.contains(&"Vec::with_capacity"));
    }

    #[test]
    fn extracts_macro_invocations() {
        let source = br#"fn example() {
    println!("hello");
    vec![1, 2, 3];
}
"#;
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"println!"));
        assert!(callees.contains(&"vec!"));
    }

    #[test]
    fn call_line_numbers_are_correct() {
        let source = br#"fn example() {
    foo();
    bar();
    baz();
}
"#;
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert_eq!(calls[0].line, 2);
        assert_eq!(calls[1].line, 3);
        assert_eq!(calls[2].line, 4);
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = RustAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Depth-guard warning: when `extract_functions_from_node` hits MAX_RECURSION_DEPTH
    /// it must emit a tracing::warn! so operators can observe truncation in logs/OTLP.
    ///
    /// Uses 300 levels of nested `impl` blocks — past MAX_RECURSION_DEPTH (256) but shallow
    /// enough to run on the default test stack without spawning a new thread. Tree-sitter
    /// Rust error recovery produces actual nested `impl_item` nodes from this invalid syntax.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_impls() {
        const NESTING_DEPTH: usize = 300;

        let mut source = String::new();
        for i in 0..NESTING_DEPTH {
            source.push_str(&format!("impl T{i} {{\n"));
        }
        source.push_str("fn leaf() {}\n");
        for _ in 0..NESTING_DEPTH {
            source.push_str("}\n");
        }

        let analyzer = RustAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_input() {
        let source = b"fn foo() {}\nfn bar() {}\n";
        let analyzer = RustAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Security regression: deeply-nested `impl` blocks (invalid Rust syntax) used to
    /// stack-overflow `extract_functions_from_node` because it recursed into each
    /// `impl_item` body without a depth limit. Although `impl Foo { impl Bar {} }` is
    /// a parse error, the tree-sitter Rust grammar's error recovery DOES produce nested
    /// `impl_item` nodes from this input — contrary to the initial plan assumption.
    /// An attacker committing such a file could crash git-prism during `get_change_manifest`.
    ///
    /// RED: 5000 nested `impl` blocks on a 2 MB bounded-stack thread → SIGABRT without guard.
    /// GREEN: guard returns early at depth 256; thread completes normally.
    ///
    /// Runs on a thread with a 2 MB stack: roomy enough for bounded recursion to
    /// `MAX_RECURSION_DEPTH` but far too small for unbounded recursion to 5000 frames.
    #[test]
    fn it_completes_without_overflow_on_deeply_nested_impls() {
        const GENERATED_NESTING_LEVELS: usize = 5000;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("impl T{i} {{\n"));
        }
        source.push_str("fn leaf() {}\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = RustAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        result.expect("analyzer must return Ok on deeply-nested input");
        // No assertion on function count — nested impls are a parse error, but
        // tree-sitter Rust error recovery DOES produce nested impl_item nodes
        // (confirmed: the unguarded walker SIGABRTs at 5000 levels on a 2MB thread).
        // The leaf function is past the depth cap and is not extracted.
    }

    /// Triangulation: 255 sequential impl blocks (not nested), each with one method.
    /// Confirms the guard does not interfere with legitimate (shallow) impl extraction.
    /// NOTE: This does NOT exercise the depth-255 boundary — valid Rust syntax does not
    /// allow nested impl blocks. However, tree-sitter Rust error recovery DOES produce
    /// nested impl_item nodes from syntactically illegal nesting (confirmed: SIGABRT at
    /// 5000 levels on a 2 MB thread). The it_completes_without_overflow_on_deeply_nested_impls
    /// test covers the overflow safety property; this test covers the non-regression property.
    #[test]
    fn sequential_impls_all_extract() {
        const IMPL_COUNT: usize = 255;

        let mut source = String::new();
        for i in 0..IMPL_COUNT {
            source.push_str(&format!("struct T{i};\n"));
            source.push_str(&format!("impl T{i} {{ fn method_{i}(&self) {{}} }}\n"));
        }

        let analyzer = RustAnalyzer;
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        assert_eq!(
            functions.len(),
            IMPL_COUNT,
            "all {IMPL_COUNT} impl methods must be extracted"
        );
    }
}
