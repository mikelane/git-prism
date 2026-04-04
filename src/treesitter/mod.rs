pub mod go;
pub mod python;
pub mod rust_lang;
pub mod typescript;

/// A function extracted from source code by tree-sitter analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        "ts" | "tsx" => Some(Box::new(typescript::TypeScriptAnalyzer)),
        "js" | "jsx" => Some(Box::new(typescript::TypeScriptAnalyzer)),
        "rs" => Some(Box::new(rust_lang::RustAnalyzer)),
        _ => None,
    }
}
