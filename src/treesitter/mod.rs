pub mod c_lang;
pub mod cpp;
pub mod go;
pub mod java;
pub mod python;
pub mod rust_lang;
pub mod typescript;

/// A function extracted from source code by tree-sitter analysis.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct Function {
    pub name: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Trait for language-specific function and import extraction.
pub trait LanguageAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>>;
    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>>;
}

/// Returns the analyzer for a file extension, or None if unsupported.
pub fn analyzer_for_extension(ext: &str) -> Option<Box<dyn LanguageAnalyzer>> {
    match ext {
        "go" => Some(Box::new(go::GoAnalyzer)),
        "py" => Some(Box::new(python::PythonAnalyzer)),
        "ts" => Some(Box::new(typescript::TypeScriptAnalyzer::typescript())),
        "tsx" => Some(Box::new(typescript::TypeScriptAnalyzer::tsx())),
        "js" | "jsx" => Some(Box::new(typescript::TypeScriptAnalyzer::javascript())),
        "rs" => Some(Box::new(rust_lang::RustAnalyzer)),
        "java" => Some(Box::new(java::JavaAnalyzer)),
        "c" | "h" => Some(Box::new(c_lang::CAnalyzer)),
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" => Some(Box::new(cpp::CppAnalyzer)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_some_for_supported_extensions() {
        for ext in &["go", "py", "ts", "tsx", "js", "jsx", "rs", "java"] {
            assert!(
                analyzer_for_extension(ext).is_some(),
                "expected Some for extension '{ext}'"
            );
        }
    }

    #[test]
    fn registry_returns_some_for_c_extensions() {
        for ext in &["c", "h"] {
            assert!(
                analyzer_for_extension(ext).is_some(),
                "expected Some for extension '{ext}'"
            );
        }
    }

    #[test]
    fn registry_returns_some_for_cpp_extensions() {
        for ext in &["cpp", "hpp"] {
            assert!(
                analyzer_for_extension(ext).is_some(),
                "expected Some for extension '{ext}'"
            );
        }
    }

    #[test]
    fn registry_returns_none_for_unsupported_extensions() {
        for ext in &["rb", "txt", ""] {
            assert!(
                analyzer_for_extension(ext).is_none(),
                "expected None for extension '{ext}'"
            );
        }
    }

    #[test]
    fn function_serializes_to_json() {
        let f = Function {
            name: "main".into(),
            signature: "fn main()".into(),
            start_line: 1,
            end_line: 3,
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["name"], "main");
        assert_eq!(json["signature"], "fn main()");
        assert_eq!(json["start_line"], 1);
        assert_eq!(json["end_line"], 3);
    }
}
