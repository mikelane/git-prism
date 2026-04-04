use super::{Function, LanguageAnalyzer};

pub struct PythonAnalyzer;

impl LanguageAnalyzer for PythonAnalyzer {
    fn extract_functions(&self, _source: &[u8]) -> anyhow::Result<Vec<Function>> {
        todo!("Implement Python function extraction with tree-sitter")
    }

    fn extract_imports(&self, _source: &[u8]) -> anyhow::Result<Vec<String>> {
        todo!("Implement Python import extraction with tree-sitter")
    }
}
