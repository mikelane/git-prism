use super::{Function, LanguageAnalyzer};

pub struct GoAnalyzer;

impl LanguageAnalyzer for GoAnalyzer {
    fn extract_functions(&self, _source: &[u8]) -> anyhow::Result<Vec<Function>> {
        todo!("Implement Go function extraction with tree-sitter")
    }

    fn extract_imports(&self, _source: &[u8]) -> anyhow::Result<Vec<String>> {
        todo!("Implement Go import extraction with tree-sitter")
    }
}
