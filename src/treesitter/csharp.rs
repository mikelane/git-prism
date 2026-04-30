use super::{CallSite, Function, LanguageAnalyzer, MAX_RECURSION_DEPTH, body_hash_for_node};
use tree_sitter::Parser;

pub struct CSharpAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
        .expect("Error loading C# grammar");
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
        if child.kind() == "method_declaration" || child.kind() == "constructor_declaration" {
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

impl LanguageAnalyzer for CSharpAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C# source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();

        // C# files may have a namespace declaration wrapping classes,
        // or classes at the top level. We need to handle both.
        //
        // `depth` bounds recursion to prevent stack overflow on adversarial
        // input (e.g., thousands of nested namespaces). Methods extracted
        // before the limit is hit are still returned.
        fn visit_node(
            source: &[u8],
            node: &tree_sitter::Node,
            functions: &mut Vec<Function>,
            depth: usize,
        ) {
            if depth >= MAX_RECURSION_DEPTH {
                tracing::warn!(
                    depth_limit = MAX_RECURSION_DEPTH,
                    language = "csharp",
                    operation = "functions",
                    "tree-sitter depth guard fired: recursive walk truncated; some functions may be missing"
                );
                return;
            }
            match node.kind() {
                "class_declaration" | "struct_declaration" | "record_declaration" => {
                    extract_methods_from_class(source, node, functions);
                }
                _ => {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        visit_node(source, &child, functions, depth + 1);
                    }
                }
            }
        }

        visit_node(source, &root, &mut functions, 0);

        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C# source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "invocation_expression"
                && let Some(expr) = node.child(0)
            {
                let callee = expr.utf8_text(source).unwrap_or("").to_string();
                let (is_method_call, receiver) = match expr.kind() {
                    "member_access_expression" => {
                        let recv = expr
                            .child_by_field_name("expression")
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse C# source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "using_directive" {
                let text = child.utf8_text(source).unwrap_or("");
                // Strip "using" keyword, optional "static" keyword, and trailing ";"
                let import_path = text
                    .trim()
                    .trim_start_matches("using")
                    .trim()
                    .trim_start_matches("static")
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
    use tracing_test::traced_test;

    #[test]
    fn extracts_simple_method() {
        let source = br#"public class Calculator {
    public int Add(int a, int b) {
        return a + b;
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.Add");
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_multiple_methods() {
        let source = br#"public class Math {
    public int Add(int a, int b) {
        return a + b;
    }

    public int Subtract(int a, int b) {
        return a - b;
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Math.Add");
        assert_eq!(functions[1].name, "Math.Subtract");
    }

    #[test]
    fn extracts_constructor() {
        let source = br#"public class Person {
    private string name;

    public Person(string name) {
        this.name = name;
    }

    public string GetName() {
        return this.name;
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Person.Person");
        assert_eq!(functions[1].name, "Person.GetName");
    }

    #[test]
    fn extracts_static_method() {
        let source = br#"public class Utils {
    public static string Format(string s) {
        return s.Trim();
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Utils.Format");
    }

    // Checks both methods in one fixture; row > 0 ensures row+1 != row*1 and row+1 != row-1.
    #[test]
    fn it_reports_correct_line_numbers_for_methods_inside_namespace() {
        let source = br#"using System;

namespace MyApp {
    public class Calculator {
        public int Add(int a, int b) {
            return a + b;
        }

        public int Subtract(int a, int b) {
            return a - b;
        }
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Calculator.Add");
        assert_eq!(functions[0].start_line, 5);
        assert_eq!(functions[0].end_line, 7);
        assert_eq!(functions[1].name, "Calculator.Subtract");
        assert_eq!(functions[1].start_line, 9);
        assert_eq!(functions[1].end_line, 11);
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_single_using() {
        let source = br#"using System;

public class Foo {}
"#;
        let analyzer = CSharpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["System"]);
    }

    #[test]
    fn extracts_multiple_usings() {
        let source = br#"using System;
using System.Collections.Generic;
using System.Linq;

public class Foo {}
"#;
        let analyzer = CSharpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec!["System", "System.Collections.Generic", "System.Linq"]
        );
    }

    #[test]
    fn extracts_static_using() {
        let source = br#"using static System.Math;

public class Foo {}
"#;
        let analyzer = CSharpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["System.Math"]);
    }

    #[test]
    fn no_usings_returns_empty() {
        let source = br#"public class Foo {
    public void Bar() {}
}
"#;
        let analyzer = CSharpAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn extracts_methods_from_namespaced_class() {
        let source = br#"namespace MyApp {
    public class Service {
        public void Run() {
            // do something
        }
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Service.Run");
    }

    #[test]
    fn extracts_method_signature() {
        let source = br#"public class Calculator {
    public int Add(int a, int b) {
        return a + b;
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert!(
            functions[0]
                .signature
                .contains("public int Add(int a, int b)")
        );
    }

    #[test]
    fn extracts_invocation_expressions() {
        let source = br#"class Example {
    void Process() {
        int x = Calculate(input);
        Console.WriteLine(x);
        helper.DoWork();
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"Calculate"));
        assert!(callees.contains(&"Console.WriteLine"));
        assert!(callees.contains(&"helper.DoWork"));
    }

    // Kill "delete match arm member_access_expression" mutant: assert that a
    // method call expressed as `receiver.Method()` is flagged is_method_call=true
    // with receiver populated. Without the member_access_expression arm, both
    // would fall through to (false, None).
    #[test]
    fn it_reports_method_call_with_receiver_for_member_access() {
        let source = br#"class Example {
    void Run() {
        helper.DoWork();
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let do_work = calls
            .iter()
            .find(|c| c.callee == "helper.DoWork")
            .expect("helper.DoWork call must be present");
        assert!(
            do_work.is_method_call,
            "member_access_expression call must be flagged as method call"
        );
        assert_eq!(do_work.receiver.as_deref(), Some("helper"));
    }

    // Kill extract_calls line-offset mutants (+ with - or *). Calls on lines 3, 4, 5
    // distinguish `row + 1` from `row * 1` and `row - 1`.
    #[test]
    fn it_reports_call_sites_on_correct_lines() {
        let source = b"class Example {
    void Run() {
        Foo();
        Bar();
        Baz();
    }
}
";
        let analyzer = CSharpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let foo = calls.iter().find(|c| c.callee == "Foo").expect("Foo call");
        let bar = calls.iter().find(|c| c.callee == "Bar").expect("Bar call");
        let baz = calls.iter().find(|c| c.callee == "Baz").expect("Baz call");
        assert_eq!(foo.line, 3);
        assert_eq!(bar.line, 4);
        assert_eq!(baz.line, 5);
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"";
        let analyzer = CSharpAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }

    /// Security regression: deeply-nested namespace declarations used to stack-overflow
    /// `visit_node` because it recursed without a depth limit. An attacker committing
    /// a C# file with thousands of nested namespaces could crash git-prism during
    /// `get_change_manifest`. The analyzer must now complete without crashing.
    ///
    /// Depth-guard warning: when `visit_node` hits MAX_RECURSION_DEPTH it must emit
    /// a tracing::warn! so operators can observe truncation in logs/OTLP.
    ///
    /// Uses 300 nesting levels — well past MAX_RECURSION_DEPTH (256) but shallow
    /// enough to run on the default test stack without spawning a new thread.
    /// `#[traced_test]` only captures logs from the current thread, so we must
    /// avoid spawning a new thread here.
    #[test]
    #[traced_test]
    fn it_emits_depth_guard_warning_on_deeply_nested_namespaces() {
        const GENERATED_NESTING_LEVELS: usize = 300;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let analyzer = CSharpAnalyzer;
        let _ = analyzer.extract_functions(source.as_bytes());
        assert!(logs_contain("depth guard fired"));
        assert!(logs_contain("language=\"csharp\""));
        assert!(logs_contain("operation=\"functions\""));
    }

    /// Triangulation: shallow input must NOT emit the depth-guard warning.
    #[test]
    #[traced_test]
    fn it_does_not_emit_depth_guard_warning_on_shallow_input() {
        let source = br#"namespace MyApp {
    public class Service {
        public void Run() {}
    }
}
"#;
        let analyzer = CSharpAnalyzer;
        let _ = analyzer.extract_functions(source);
        assert!(!logs_contain("depth guard fired"));
    }

    /// Uses nested namespaces (not classes) because `visit_node` stops recursing at
    /// a `class_declaration`, but falls through and recurses into children for any
    /// other node kind — including `namespace_declaration`.
    ///
    /// Runs on a thread with a 2 MB stack: roomy enough for bounded recursion to
    /// `MAX_RECURSION_DEPTH` but far too small for unbounded recursion to 5000 frames.
    #[test]
    fn deeply_nested_namespaces_do_not_stack_overflow() {
        const GENERATED_NESTING_LEVELS: usize = 5000;
        const CONSTRAINED_THREAD_STACK_BYTES: usize = 2 * 1024 * 1024;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let handle = std::thread::Builder::new()
            .stack_size(CONSTRAINED_THREAD_STACK_BYTES)
            .spawn(move || {
                let analyzer = CSharpAnalyzer;
                analyzer.extract_functions(source.as_bytes())
            })
            .expect("spawn analyzer thread");

        let result = handle
            .join()
            .expect("analyzer thread must not stack-overflow on deeply-nested input");
        let functions = result.expect("analyzer must return Ok on deeply-nested input");
        // Namespaces contain no methods, so no functions should be extracted.
        assert!(functions.is_empty());
    }

    /// Triangulation: moderately-nested namespaces with a class at the innermost level
    /// must still be extracted — the depth guard must not fire on legitimate code.
    ///
    /// C#'s `visit_node` recurses into ALL children of non-class nodes, so each
    /// semantic namespace level consumes several depth increments. This test uses
    /// 10 levels — well within any reasonable limit — to prove the guard does not
    /// erroneously block legitimate real-world code.
    #[test]
    fn it_extracts_class_from_moderately_nested_namespaces() {
        const GENERATED_NESTING_LEVELS: usize = 10;

        let mut source = String::new();
        for i in 0..GENERATED_NESTING_LEVELS {
            source.push_str(&format!("namespace N{i} {{\n"));
        }
        source.push_str("public class Leaf { public void Run() {} }\n");
        for _ in 0..GENERATED_NESTING_LEVELS {
            source.push_str("}\n");
        }

        let analyzer = CSharpAnalyzer;
        let functions = analyzer.extract_functions(source.as_bytes()).unwrap();
        let leaf = functions.iter().find(|f| f.name == "Leaf.Run");
        assert!(
            leaf.is_some(),
            "method inside class nested 10 levels deep must be extracted; got {} functions",
            functions.len()
        );
    }
}
