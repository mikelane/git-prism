pub mod c_lang;
pub mod cpp;
pub mod csharp;
pub mod go;
pub mod java;
pub mod kotlin;
pub mod php;
pub mod python;
pub mod ruby;
pub mod rust_lang;
pub mod swift;
pub mod typescript;

use sha2::{Digest, Sha256};

/// Hex-encode the finalized digest of a hasher.
///
/// sha2 0.11 returned `hybrid_array::Array<u8, _>` from `finalize()`, which —
/// unlike the previous `generic_array::GenericArray` — does not implement
/// `LowerHex`. So `format!("{:x}", hasher.finalize())` no longer compiles.
/// This helper produces byte-equivalent output via a hand-rolled hex loop,
/// avoiding a new dependency for ~6 lines of formatting.
pub fn finalize_hex<D: Digest>(hasher: D) -> String {
    use std::fmt::Write;
    let bytes = hasher.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes.iter() {
        write!(out, "{:02x}", b).expect("writing to String never fails");
    }
    out
}

/// Maximum recursion depth for tree-walking analyzers.
///
/// Treesitter analyzers that recurse through nested declarations (nested
/// namespaces, classes, modules, etc.) must bound their recursion to avoid
/// stack overflow when processing adversarial input read from git blobs.
///
/// An attacker committing a source file with thousands of nested class or
/// module blocks would otherwise crash git-prism during `get_change_manifest`.
///
/// 256 is generous for real-world code — humans rarely nest more than ~20
/// levels — while keeping worst-case stack usage comfortably under the
/// default 2 MB cargo test stack and the 8 MB default main thread stack.
pub const MAX_RECURSION_DEPTH: usize = 256;

/// Compute a hex-encoded SHA-256 hash of the given bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    finalize_hex(hasher)
}

/// Hash a function node's body for content-aware diffing.
///
/// Tries `child_by_field_name("body")` first; falls back to hashing the
/// entire node (correct for bodyless constructs like forward declarations).
pub fn body_hash_for_node(source: &[u8], node: tree_sitter::Node) -> String {
    let body_node = node.child_by_field_name("body").unwrap_or(node);
    sha256_hex(&source[body_node.start_byte()..body_node.end_byte()])
}

/// A function extracted from source code by tree-sitter analysis.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct Function {
    pub name: String,
    pub signature: String,
    pub start_line: usize,
    pub end_line: usize,
    /// SHA-256 hash of the function body bytes. Internal use only.
    #[serde(skip)]
    #[schemars(skip)]
    pub body_hash: String,
}

/// A function call site extracted from source code by tree-sitter analysis.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct CallSite {
    /// The full callee expression (e.g., "foo", "self.bar", "pkg::func").
    pub callee: String,
    /// 1-indexed line number where the call occurs.
    pub line: usize,
    /// Whether this is a method call (has a receiver).
    pub is_method_call: bool,
    /// The receiver expression, if this is a method call (e.g., "self", "server").
    pub receiver: Option<String>,
}

/// Trait for language-specific function and import extraction.
pub trait LanguageAnalyzer {
    fn extract_functions(&self, source: &[u8]) -> anyhow::Result<Vec<Function>>;
    fn extract_imports(&self, source: &[u8]) -> anyhow::Result<Vec<String>>;
    fn extract_calls(&self, source: &[u8]) -> anyhow::Result<Vec<CallSite>> {
        let _ = source;
        Ok(vec![])
    }
}

/// Returns the analyzer for a file extension, or None if unsupported.
pub fn analyzer_for_extension(ext: &str) -> Option<Box<dyn LanguageAnalyzer>> {
    match ext {
        "go" => Some(Box::new(go::GoAnalyzer)),
        "py" => Some(Box::new(python::PythonAnalyzer)),
        "ts" => Some(Box::new(typescript::TypeScriptAnalyzer::typescript())),
        "tsx" => Some(Box::new(typescript::TypeScriptAnalyzer::tsx())),
        "js" | "jsx" => Some(Box::new(typescript::TypeScriptAnalyzer::javascript())),
        "rb" => Some(Box::new(ruby::RubyAnalyzer)),
        "rs" => Some(Box::new(rust_lang::RustAnalyzer)),
        "java" => Some(Box::new(java::JavaAnalyzer)),
        "php" => Some(Box::new(php::PhpAnalyzer)),
        "swift" => Some(Box::new(swift::SwiftAnalyzer)),
        "kt" | "kts" => Some(Box::new(kotlin::KotlinAnalyzer)),
        "c" | "h" => Some(Box::new(c_lang::CAnalyzer)),
        "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" => Some(Box::new(cpp::CppAnalyzer)),
        "cs" => Some(Box::new(csharp::CSharpAnalyzer)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_some_for_supported_extensions() {
        for ext in &[
            "go", "py", "ts", "tsx", "js", "jsx", "rs", "java", "php", "cs", "rb", "swift", "kt",
            "kts",
        ] {
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
        for ext in &["cpp", "hpp", "cc", "cxx", "hh", "hxx"] {
            assert!(
                analyzer_for_extension(ext).is_some(),
                "expected Some for extension '{ext}'"
            );
        }
    }

    #[test]
    fn registry_returns_some_for_ruby_extension() {
        assert!(
            analyzer_for_extension("rb").is_some(),
            "expected Some for extension 'rb'"
        );
    }

    #[test]
    fn registry_returns_some_for_kotlin_extensions() {
        for ext in &["kt", "kts"] {
            assert!(
                analyzer_for_extension(ext).is_some(),
                "expected Some for extension '{ext}'"
            );
        }
    }

    #[test]
    fn registry_returns_none_for_unsupported_extensions() {
        for ext in &["txt", ""] {
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
            body_hash: "abc123".into(),
        };
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["name"], "main");
        assert_eq!(json["signature"], "fn main()");
        assert_eq!(json["start_line"], 1);
        assert_eq!(json["end_line"], 3);
        // body_hash is internal plumbing — must NOT leak into JSON output
        assert!(
            json.get("body_hash").is_none(),
            "body_hash must be excluded from serialization"
        );
    }

    #[test]
    fn callsite_serializes_to_json() {
        let cs = CallSite {
            callee: "foo".into(),
            line: 10,
            is_method_call: false,
            receiver: None,
        };
        let json = serde_json::to_value(&cs).unwrap();
        assert_eq!(json["callee"], "foo");
        assert_eq!(json["line"], 10);
        assert_eq!(json["is_method_call"], false);
        assert!(json["receiver"].is_null());
    }

    #[test]
    fn callsite_with_receiver_serializes() {
        let cs = CallSite {
            callee: "server.start".into(),
            line: 5,
            is_method_call: true,
            receiver: Some("server".into()),
        };
        let json = serde_json::to_value(&cs).unwrap();
        assert_eq!(json["callee"], "server.start");
        assert_eq!(json["is_method_call"], true);
        assert_eq!(json["receiver"], "server");
    }
}
