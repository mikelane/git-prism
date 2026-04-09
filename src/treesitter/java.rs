use super::{CallSite, Function, LanguageAnalyzer, body_hash_for_node};
use tree_sitter::Parser;

pub struct JavaAnalyzer;

fn create_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .expect("Error loading Java grammar");
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

impl LanguageAnalyzer for JavaAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Java source"))?;
        let root = tree.root_node();
        let mut functions = Vec::new();

        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "class_declaration" {
                extract_methods_from_class(source, &child, &mut functions);
            }
        }

        Ok(functions)
    }

    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let mut parser = create_parser();
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Java source"))?;

        let mut calls = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "method_invocation" {
                let name = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                let (callee, is_method_call, receiver) =
                    if let Some(obj) = node.child_by_field_name("object") {
                        let obj_text = obj.utf8_text(source).unwrap_or("").to_string();
                        (format!("{obj_text}.{name}"), true, Some(obj_text))
                    } else {
                        (name.to_string(), false, None)
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
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Java source"))?;
        let root = tree.root_node();
        let mut imports = Vec::new();

        // Imports are stripped to bare package paths (e.g., "java.util.List") rather than
        // preserving the full statement text ("import java.util.List;"). Java import syntax
        // is verbose and uniform -- the package path is the meaningful, differentiating part.
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "import_declaration" {
                let text = child.utf8_text(source).unwrap_or("");
                let import_path = text
                    .trim()
                    .trim_start_matches("import")
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

    #[test]
    fn extracts_simple_method() {
        let source = br#"public class Calculator {
    public int add(int a, int b) {
        return a + b;
    }
}
"#;
        let analyzer = JavaAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Calculator.add");
        assert_eq!(functions[0].signature, "public int add(int a, int b)");
        assert_eq!(functions[0].start_line, 2);
        assert_eq!(functions[0].end_line, 4);
    }

    #[test]
    fn extracts_multiple_methods() {
        let source = br#"public class Math {
    public int add(int a, int b) {
        return a + b;
    }

    public int subtract(int a, int b) {
        return a - b;
    }
}
"#;
        let analyzer = JavaAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Math.add");
        assert_eq!(functions[1].name, "Math.subtract");
        assert_eq!(functions[1].signature, "public int subtract(int a, int b)");
    }

    #[test]
    fn extracts_constructor() {
        let source = br#"public class Person {
    private String name;

    public Person(String name) {
        this.name = name;
    }

    public String getName() {
        return this.name;
    }
}
"#;
        let analyzer = JavaAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 2);
        assert_eq!(functions[0].name, "Person.Person");
        assert_eq!(functions[0].signature, "public Person(String name)");
        assert_eq!(functions[1].name, "Person.getName");
    }

    #[test]
    fn extracts_static_method() {
        let source = br#"public class Utils {
    public static String format(String s) {
        return s.trim();
    }
}
"#;
        let analyzer = JavaAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Utils.format");
        assert_eq!(
            functions[0].signature,
            "public static String format(String s)"
        );
    }

    #[test]
    fn empty_file_returns_no_functions() {
        let source = b"";
        let analyzer = JavaAnalyzer;
        let functions = analyzer.extract_functions(source).unwrap();
        assert!(functions.is_empty());
    }

    #[test]
    fn extracts_single_import() {
        let source = br#"import java.util.List;

public class Foo {}
"#;
        let analyzer = JavaAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["java.util.List"]);
    }

    #[test]
    fn extracts_multiple_imports() {
        let source = br#"import java.util.List;
import java.util.Map;
import java.io.IOException;

public class Foo {}
"#;
        let analyzer = JavaAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(
            imports,
            vec!["java.util.List", "java.util.Map", "java.io.IOException"]
        );
    }

    #[test]
    fn extracts_static_import() {
        let source = br#"import static java.util.Collections.emptyList;

public class Foo {}
"#;
        let analyzer = JavaAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["java.util.Collections.emptyList"]);
    }

    #[test]
    fn extracts_wildcard_import() {
        let source = br#"import java.util.*;

public class Foo {}
"#;
        let analyzer = JavaAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert_eq!(imports, vec!["java.util.*"]);
    }

    #[test]
    fn no_imports_returns_empty() {
        let source = br#"public class Foo {
    public void bar() {}
}
"#;
        let analyzer = JavaAnalyzer;
        let imports = analyzer.extract_imports(source).unwrap();
        assert!(imports.is_empty());
    }

    #[test]
    fn extracts_method_invocations() {
        let source = br#"public class Example {
    public void process() {
        int x = calculate(input);
        System.out.println(x);
        helper.doWork();
    }
}
"#;
        let analyzer = JavaAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        let callees: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"calculate"));
        assert!(callees.contains(&"System.out.println"));
        assert!(callees.contains(&"helper.doWork"));
    }

    #[test]
    fn empty_file_returns_no_calls() {
        let source = b"public class Foo {}";
        let analyzer = JavaAnalyzer;
        let calls = analyzer.extract_calls(source).unwrap();
        assert!(calls.is_empty());
    }
}
