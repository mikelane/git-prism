fn main() {
    let kotlin_dir = std::path::Path::new("vendor/tree-sitter-kotlin/src");

    let mut config = cc::Build::new();
    config.include(kotlin_dir);
    config
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs");
    config.file(kotlin_dir.join("parser.c"));
    config.file(kotlin_dir.join("scanner.c"));
    config.compile("tree_sitter_kotlin");

    println!("cargo:rerun-if-changed=vendor/tree-sitter-kotlin/src/parser.c");
    println!("cargo:rerun-if-changed=vendor/tree-sitter-kotlin/src/scanner.c");
}
