//! Spike: Validate tree-sitter body node ranges for content-aware function diffs.
//!
//! This test validates that we can reliably extract function body byte ranges
//! from tree-sitter ASTs across all supported languages, and that SHA-256
//! hashing of those ranges is deterministic.
//!
//! This is throwaway spike code. Delete after ADR 0004 is written.

use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Parse source with the given tree-sitter language.
fn parse(lang: tree_sitter::Language, source: &[u8]) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).unwrap();
    parser.parse(source, None).unwrap()
}

/// Walk the tree to find a function-like node starting at `target_line` (1-indexed),
/// and return the byte range of its body child node.
fn find_body_range(
    node: &tree_sitter::Node,
    target_line: usize,
    function_kinds: &[&str],
) -> Option<(usize, usize)> {
    if function_kinds.contains(&node.kind()) && node.start_position().row + 1 == target_line {
        // Try "body" field first (works for 12 of 13 languages)
        if let Some(body) = node.child_by_field_name("body") {
            return Some((body.start_byte(), body.end_byte()));
        }
        // Fallback: look for body-like children by kind (Kotlin, etc.)
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "function_body" | "block" | "compound_statement"
            ) {
                return Some((child.start_byte(), child.end_byte()));
            }
        }
        return None;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(range) = find_body_range(&child, target_line, function_kinds) {
            return Some(range);
        }
    }
    None
}

// ---- Per-language body node validation ----

#[test]
fn spike_rust_body_node() {
    let src = b"fn greet(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n";
    let tree = parse(tree_sitter_rust::LANGUAGE.into(), src);
    let body = find_body_range(&tree.root_node(), 1, &["function_item"]);
    assert!(body.is_some(), "Rust: body node found");
    let (s, e) = body.unwrap();
    let text = std::str::from_utf8(&src[s..e]).unwrap();
    assert!(text.starts_with('{'), "Rust: body starts with brace");
    assert!(
        text.contains("format!"),
        "Rust: body contains implementation"
    );
}

#[test]
fn spike_rust_hash_deterministic_and_position_independent() {
    let v1 = b"fn foo() {\n    println!(\"hello\");\n}\n";
    let v2 = b"fn bar() {}\n\nfn foo() {\n    println!(\"hello\");\n}\n";

    let tree1 = parse(tree_sitter_rust::LANGUAGE.into(), v1);
    let tree2 = parse(tree_sitter_rust::LANGUAGE.into(), v2);

    // foo is at line 1 in v1, line 3 in v2
    let body1 = find_body_range(&tree1.root_node(), 1, &["function_item"]).unwrap();
    let body2 = find_body_range(&tree2.root_node(), 3, &["function_item"]).unwrap();

    let hash1 = sha256_hex(&v1[body1.0..body1.1]);
    let hash2 = sha256_hex(&v2[body2.0..body2.1]);
    assert_eq!(hash1, hash2, "same body at different positions → same hash");
}

#[test]
fn spike_rust_hash_differs_on_body_change() {
    let v1 = b"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    let v2 = b"fn add(a: i32, b: i32) -> i32 {\n    a * b\n}\n";

    let tree1 = parse(tree_sitter_rust::LANGUAGE.into(), v1);
    let tree2 = parse(tree_sitter_rust::LANGUAGE.into(), v2);

    let body1 = find_body_range(&tree1.root_node(), 1, &["function_item"]).unwrap();
    let body2 = find_body_range(&tree2.root_node(), 1, &["function_item"]).unwrap();

    let hash1 = sha256_hex(&v1[body1.0..body1.1]);
    let hash2 = sha256_hex(&v2[body2.0..body2.1]);
    assert_ne!(hash1, hash2, "different body → different hash");
}

#[test]
fn spike_python_body_node() {
    let src = b"def greet(name):\n    return f\"Hello, {name}!\"\n";
    let tree = parse(tree_sitter_python::LANGUAGE.into(), src);
    let body = find_body_range(&tree.root_node(), 1, &["function_definition"]);
    assert!(body.is_some(), "Python: body node found");
    let (s, e) = body.unwrap();
    let text = std::str::from_utf8(&src[s..e]).unwrap();
    assert!(text.contains("return"), "Python: body contains return");
}

#[test]
fn spike_go_body_node() {
    let src = b"package main\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n";
    let tree = parse(tree_sitter_go::LANGUAGE.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        3,
        &["function_declaration", "method_declaration"],
    );
    assert!(body.is_some(), "Go: body node found");
}

#[test]
fn spike_typescript_body_node() {
    let src = b"function greet(name: string): string {\n  return `Hello, ${name}!`;\n}\n";
    let tree = parse(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        1,
        &["function_declaration", "method_definition"],
    );
    assert!(body.is_some(), "TypeScript: body node found");
}

#[test]
fn spike_javascript_body_node() {
    let src = b"function greet(name) {\n  return 'Hello, ' + name;\n}\n";
    let tree = parse(tree_sitter_javascript::LANGUAGE.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        1,
        &["function_declaration", "method_definition"],
    );
    assert!(body.is_some(), "JavaScript: body node found");
}

#[test]
fn spike_java_body_node() {
    let src =
        b"class Greeter {\n  public String greet(String name) {\n    return \"Hello\";\n  }\n}\n";
    let tree = parse(tree_sitter_java::LANGUAGE.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        2,
        &["method_declaration", "constructor_declaration"],
    );
    assert!(body.is_some(), "Java: body node found");
}

#[test]
fn spike_c_body_node() {
    let src = b"int add(int a, int b) {\n    return a + b;\n}\n";
    let tree = parse(tree_sitter_c::LANGUAGE.into(), src);
    let body = find_body_range(&tree.root_node(), 1, &["function_definition"]);
    assert!(body.is_some(), "C: body node found");
}

#[test]
fn spike_c_declaration_no_body() {
    let src = b"int add(int a, int b);\n";
    let tree = parse(tree_sitter_c::LANGUAGE.into(), src);
    // A declaration node kind is "declaration", not "function_definition"
    let body = find_body_range(&tree.root_node(), 1, &["function_definition"]);
    assert!(body.is_none(), "C: forward declaration has no body");
}

#[test]
fn spike_cpp_body_node() {
    let src = b"int add(int a, int b) {\n    return a + b;\n}\n";
    let tree = parse(tree_sitter_cpp::LANGUAGE.into(), src);
    let body = find_body_range(&tree.root_node(), 1, &["function_definition"]);
    assert!(body.is_some(), "C++: body node found");
}

#[test]
fn spike_php_body_node() {
    let src = b"<?php\nfunction greet($name) {\n    return \"Hello, \" . $name;\n}\n";
    let tree = parse(tree_sitter_php::LANGUAGE_PHP.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        2,
        &["function_definition", "method_declaration"],
    );
    assert!(body.is_some(), "PHP: body node found");
}

#[test]
fn spike_csharp_body_node() {
    let src = b"class Greeter {\n    public string Greet(string name) {\n        return \"Hello\";\n    }\n}\n";
    let tree = parse(tree_sitter_c_sharp::LANGUAGE.into(), src);
    let body = find_body_range(
        &tree.root_node(),
        2,
        &["method_declaration", "constructor_declaration"],
    );
    assert!(body.is_some(), "C#: body node found");
}

#[test]
fn spike_ruby_body_node() {
    let src = b"def greet(name)\n  \"Hello, #{name}!\"\nend\n";
    let tree = parse(tree_sitter_ruby::LANGUAGE.into(), src);
    let body = find_body_range(&tree.root_node(), 1, &["method", "singleton_method"]);
    assert!(body.is_some(), "Ruby: body node found");
}

// Kotlin and Swift use vendored grammars that aren't directly accessible
// as tree_sitter::Language from their crate. We validate them indirectly:
// the signature_text() function in each analyzer already uses
// child_by_field_name("body") (or the Kotlin fallback), and existing
// tests prove it works. The spike validates that the PATTERN works —
// the vendored grammars follow the same pattern.

#[test]
fn spike_sha256_basics() {
    let hash = sha256_hex(b"hello world");
    assert_eq!(hash.len(), 64, "SHA-256 hex digest is 64 chars");
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "all hex chars");

    // Same input → same output
    assert_eq!(sha256_hex(b"test"), sha256_hex(b"test"));

    // Different input → different output
    assert_ne!(sha256_hex(b"test1"), sha256_hex(b"test2"));
}
