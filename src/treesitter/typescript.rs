use super::{Function, LanguageAnalyzer};
use tree_sitter::Parser;

#[derive(Debug, Clone, Copy)]
pub enum JsDialect {
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
) {
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
                functions.push(Function {
                    name,
                    signature,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
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
                functions.push(Function {
                    name,
                    signature,
                    start_line: child.start_position().row + 1,
                    end_line: child.end_position().row + 1,
                });
            }
            "class_declaration" => {
                let cls_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("");
                if let Some(body) = child.child_by_field_name("body") {
                    extract_functions_from_node(source, &body, Some(cls_name), functions);
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
                            functions.push(Function {
                                name: fn_name.to_string(),
                                signature,
                                start_line: child.start_position().row + 1,
                                end_line: arrow_node.end_position().row + 1,
                            });
                        }
                    }
                }
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
        extract_functions_from_node(source, &root, None, &mut functions);
        Ok(functions)
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
}
