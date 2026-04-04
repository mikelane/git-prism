use super::{Function, LanguageAnalyzer};

pub struct RustAnalyzer;

impl LanguageAnalyzer for RustAnalyzer {
    fn extract_functions(&self, _source: &[u8]) -> anyhow::Result<Vec<Function>> {
        todo!("Implement Rust function extraction with tree-sitter")
    }

    fn extract_imports(&self, _source: &[u8]) -> anyhow::Result<Vec<String>> {
        todo!("Implement Rust import extraction with tree-sitter")
    }
}
