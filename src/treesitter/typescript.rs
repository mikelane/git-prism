use super::{Function, LanguageAnalyzer};

pub struct TypeScriptAnalyzer;

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn extract_functions(&self, _source: &[u8]) -> anyhow::Result<Vec<Function>> {
        todo!("Implement TypeScript/JavaScript function extraction with tree-sitter")
    }

    fn extract_imports(&self, _source: &[u8]) -> anyhow::Result<Vec<String>> {
        todo!("Implement TypeScript/JavaScript import extraction with tree-sitter")
    }
}
