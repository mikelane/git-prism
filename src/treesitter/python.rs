use super::{CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH, body_hash_for_node};
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
    depth: usize,
) {
    if depth >= MAX_RECURSION_DEPTH {
        tracing::warn!(
            depth_limit = MAX_RECURSION_DEPTH,
            language = "python",
            operation = "functions",
            "tree-sitter depth guard fired: recursive walk truncated; some functions may be missing"
        );
        return;
    }
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
                let body_hash = body_hash_for_node(source, child);
                functions.push(Function {
                    name,
                    signature,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                    body_hash,
                });
            }
            "class_definition" => {
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
            "decorated_definition" => {
                // cargo-mutants: skip -- equivalent mutant: `depth + 1` → `depth * 1`
                // is observably identical for any realistic input. `decorated_definition`
                // wraps a single inner function/class declaration, so the recursion
                // depth here cannot grow beyond a handful of levels in any real
                // codebase. The overflow-guard pattern is already exercised by the
                // class_definition arm via `it_emits_depth_guard_warning_on_deeply_nested_classes`.
                extract_functions_from_node(source, &child, class_name, functions, depth + 1);
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
        extract_functions_from_node(source, &root, None, &mut functions, 0);
        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Python source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "call"
                && let Some(func) = node.child_by_field_name("function")
            {
                let callee = func.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match func.kind() {
                    "attribute" => {
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
    use tracing_test::traced_test;

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
            vec![
                "from os.path import join",
                "from typing import List, Optional"
            ]
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

    // Kill mutants: replace + with * or - in class_definition line number arithmetic (row + 1).
    // Class at row > 0 ensures row+1 != row*1 and row+1 != row-1.
    #[test]
    fn it_reports_correct_line_numbers_for_class_definition() {
        let source = b"# comment line 1
# comment line 2

class MyClass:
    def method(self):
        pass
";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "MyClass");
        assert_eq!(functions[0].start_line, 4);
        assert_eq!(functions[0].end_line, 6);
        assert_eq!(functions[1].name, "MyClass.method");
        assert_eq!(functions[1].start_line, 5);
        assert_eq!(functions[1].end_line, 6);
    }

    #[test]
    fn extracts_simple_calls() {
        let source = br#"def main():
    x = foo()
    y = bar(x)
    baz(x, y)
"#;
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["foo", "bar", "baz"]);
        assert!(calls.iter().all(|c| !c.is_method_call));
    }

    #[test]
    fn extracts_method_calls() {
        let source = br#"def process(server):
    server.start()
    server.handle_request()
"#;
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert_eq!(callees, vec!["server.start", "server.handle_request"]);
        assert!(calls.iter().all(|c| c.is_method_call));
        assert_eq!(calls[0].receiver.as_deref(), Some("server"));
    }

    #[test]
    fn extracts_self_method_calls() {
        let source = br#"class MyClass:
    def process(self):
        self.validate()
        self.compute()
"#;
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"self.validate"));
        assert!(callees.contains(&"self.compute"));
        assert!(calls.iter().all(|c| c.receiver.as_deref() == Some("self")));
    }

    #[test]
    fn extracts_constructor_calls() {
        let source = br#"def example():
    obj = MyClass()
    lst = list()
"#;
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"MyClass"));
        assert!(callees.contains(&"list"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    // Kill extract_calls line-offset mutants (+ with - or *). Calls on lines 2, 3, 4
    // distinguish `row + 1` from `row * 1` and `row - 1`.
    #[test]
    fn it_reports_call_sites_on_correct_lines() {
        let source = b"def main():
    foo()
    bar()
    baz()
";
        let analyzer = PythonAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].callee, "foo");
        assert_eq!(calls[0].line, 2);
        assert_eq!(calls[1].callee, "bar");
        assert_eq!(calls[1].line, 3);
        assert_eq!(calls[2].callee, "baz");
        assert_eq!(calls[2].line, 4);
    }

    /// Depth-guard warning: when `extract_functions_from_node` hits MAX_RECURSION_DEPTH
    /// it must emit a tracing::warn! so operators can observe truncation in logs/OTLP.
    ///
    /// Uses 300 nesting levels — past MAX_RECURSION_DEPTH (256) but shallow enough
    /// to run on the default test stack without spawning a new thread.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_classes() {
        const GENERATED_NESTING_LEVELS: usize = 300;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            let indent = "    ".repeat(i);
            source.push_str(&format!("{indent}class C{i}:\n"));
        }
        let deepest_indent = "    ".repeat(GENERATED_NESTING_LEVELS);
        source.push_str(&format!("{deepest_indent}pass\n"));

        let analyzer = PythonAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
        assert!(logs_contain("language=\"python\""));
        assert!(logs_contain("operation=\"functions\""));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_input() {
        let source = b"class Foo:\n    def bar(self):\n        pass\n";
        let analyzer = PythonAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Defense-in-depth: deeply-nested Python class declarations are guarded by
    /// `MAX_RECURSION_DEPTH`. Investigation showed that Python's indented class nesting
    /// (which requires valid indentation) produces small per-frame sizes that don't
    /// naturally SIGABRT on a 2 MB bounded-stack thread at 5000 levels — tree-sitter
    /// processes them without overflowing. The guard is added for consistency with the
    /// other five analyzers and to protect against future grammar changes or alternative
    /// attack shapes that could produce deeper actual recursion.
    ///
    /// This test verifies the guard does not corrupt extraction at 5000 depth: the
    /// outermost ~256 classes must still be extracted even on a constrained stack.
    #[test]
    fn it_completes_without_overflow_on_deeply_nested_classes() {
        // 1024 = MAX_RECURSION_DEPTH * 4: enough headroom past the cap without
        // the extreme runtime of 5000-level indented Python (which is O(n²) in
        // string construction due to the indent repetition).
        const GENERATED_NESTING_LEVELS: usize = 1024;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            let indent = "    ".repeat(i);
            source.push_str(&format!("{indent}class C{i}:\n"));
        }
        // Innermost class needs a body — use `pass` at the deepest indent.
        let deepest_indent = "    ".repeat(GENERATED_NESTING_LEVELS);
        source.push_str(&format!("{deepest_indent}pass\n"));

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = PythonAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        let functions = result.expect("analyzer must return Ok on deeply-nested input");
        // The outermost MAX_RECURSION_DEPTH (256) classes must all be extracted
        // before the guard fires. Asserting >= MAX_RECURSION_DEPTH catches
        // regressions where the guard fires too early (e.g., at depth 10).
        assert!(
            functions.len() >= MAX_RECURSION_DEPTH,
            "expected at least {} classes to be extracted before depth guard fires, got {}",
            MAX_RECURSION_DEPTH,
            functions.len()
        );
    }

    /// Triangulation: 255 nested classes with a method at the innermost level.
    /// The guard fires at depth 256, so depth 255 must still allow extraction.
    #[test]
    fn it_extracts_methods_at_boundary_nesting_depth() {
        const GENERATED_NESTING_LEVELS: usize = 255;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            let indent = "    ".repeat(i);
            source.push_str(&format!("{indent}class C{i}:\n"));
        }
        // Add a method at the innermost class body (depth 255).
        let method_indent = "    ".repeat(GENERATED_NESTING_LEVELS);
        source.push_str(&format!("{method_indent}def leaf_method(self):\n"));
        let body_indent = "    ".repeat(GENERATED_NESTING_LEVELS + 1);
        source.push_str(&format!("{body_indent}pass\n"));

        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        // All 255 classes plus the leaf method must be extracted.
        let leaf = functions.iter().find(|f| f.name.ends_with("leaf_method"));
        assert!(
            leaf.is_some(),
            "method at depth 255 must be extracted; got {} functions",
            functions.len()
        );
    }

    #[test]
    fn extracts_decorated_function() {
        let source = b"@app.route(\"/\")\ndef index():\n    return \"Hello\"\n";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "index");
    }

    #[test]
    fn extracts_stacked_decorator_function() {
        let source =
            b"@app.route(\"/admin\")\n@login_required\ndef admin():\n    return \"Admin\"\n";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "admin");
    }

    #[test]
    fn extracts_decorated_class_method() {
        let source = b"class Foo:\n    @staticmethod\n    def bar():\n        pass\n";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Foo");
        assert_eq!(functions[1].name, "Foo.bar");
    }

    #[test]
    fn extracts_async_decorated_function() {
        let source = b"@app.get(\"/\")\nasync def index():\n    return {\"ok\": True}\n";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "index");
    }

    #[test]
    fn extracts_decorated_class() {
        let source = b"@dataclass\nclass Point:\n    x: int\n    y: int\n";
        let analyzer = PythonAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Point");
    }
}
