# ADR 0006: Import Resolution for Caller Scoping

- **Status**: Accepted
- **Date**: 2026-04-10
- **Epic**: #127 — Import-aware caller scoping

## Context

`get_function_context` scans every file in the repo to find callers of changed functions. This brute-force approach produces false positives (leaf-name collisions across modules) and scales poorly (O(all files) parse time). We investigated whether import data can scope the scan to only files that plausibly reference the changed module.

The question: for each of the top 4 languages, can we (a) infer a module identifier from a file path, (b) extract the module target from an import statement, and (c) match them reliably enough to filter the scan?

## Analysis by Language

### Rust

**File path → module path**: Deterministic within a crate. `src/foo/bar.rs` → `crate::foo::bar`. `src/foo/mod.rs` → `crate::foo`. The `src/` prefix maps to `crate::`.

**Import extraction**: `extract_imports()` returns `use crate::foo::bar;`, `use super::baz;`, `use std::io;`. The crate path (`crate::foo::bar`) directly maps to file paths.

**Matching**: Extract the first two segments of the `use` path after `crate::`. Compare against the changed file's inferred module path. `use crate::foo::bar::Thing;` references module `crate::foo::bar` → file `src/foo/bar.rs`.

**Edge cases**:
- `use super::` requires knowing the importer's position in the module tree — resolvable from the importer's file path.
- `use self::` refers to the same module — always matches.
- Re-exports (`pub use`) can alias module paths. Rare in practice and acceptable as a false negative.
- External crates (`use serde::Serialize`) — never match internal files, easily filtered by checking if the first segment is `crate`, `super`, or `self`.

**Verdict**: **Works.** High confidence, deterministic mapping.

### Python

**File path → module path**: `src/validation.py` → `src.validation`. `utils/__init__.py` → `utils`. Directory-based packages with `__init__.py` markers.

**Import extraction**: `extract_imports()` returns `import os`, `from src.validation import check`. The `from` clause contains the dotted module path.

**Matching**: Extract the dotted path from `from X import Y` or `import X`. Compare against the changed file's inferred dotted path. `from src.validation import check` references `src.validation` → `src/validation.py`.

**Edge cases**:
- Relative imports (`from . import foo`, `from ..utils import bar`) require knowing the importer's package position. Resolvable from file path + package structure.
- Dynamic imports (`importlib.import_module("foo")`) — unresolvable, must fall back.
- `import *` — we know the source module, so this still works for filtering.
- Missing `__init__.py` (implicit namespace packages, PEP 420) — directory still maps to dotted path.

**Verdict**: **Works with heuristics.** Direct imports are reliable. Relative imports need the importer's position. Dynamic imports require fallback.

### Go

**File path → module path**: Go is directory-based. All `.go` files in `internal/parser/` belong to whatever package is declared in their `package` line. The import path is the module path (from `go.mod`) + the directory path. E.g., if `go.mod` says `module github.com/foo/bar`, then `internal/parser/parser.go` is imported as `github.com/foo/bar/internal/parser`.

**Import extraction**: `extract_imports()` returns bare paths like `"fmt"`, `"github.com/foo/bar/internal/parser"`. These are the exact import paths.

**Matching**: For internal packages, the import path suffix after the module name matches the directory path. `"github.com/foo/bar/internal/parser"` → directory `internal/parser/`. All `.go` files in that directory are part of that package.

**Edge cases**:
- Requires reading `go.mod` to get the module name — one extra file read, always present.
- Vendor directories (`vendor/`) mirror external packages — should be excluded from scan.
- `_test.go` files may have `package foo_test` — same directory, different package name, but same import path for white-box tests.
- Go's import path is always explicit and absolute — no relative imports, no ambiguity.

**Verdict**: **Works.** Excellent fit. Import paths are explicit and deterministic. Requires `go.mod` read.

### TypeScript / JavaScript

**File path → module path**: Relative imports use file paths directly: `import { foo } from './utils'` → `./utils.ts` or `./utils/index.ts` relative to the importer. The path in the import statement is a relative file path.

**Import extraction**: `extract_imports()` returns full statements like `import { foo } from './utils';`. The module specifier is extractable with a simple regex or tree-sitter query on the `source` field.

**Matching**: For relative imports (`./`, `../`), resolve the path relative to the importing file's directory. `import { x } from '../validation'` in `src/handlers/api.ts` references `src/validation.ts`. Compare against changed file paths.

**Edge cases**:
- Path aliases (`@/utils`, `~/components`) from `tsconfig.json` `paths` — require reading tsconfig. Common in large projects. Could fall back for these.
- Bare specifiers (`import React from 'react'`) — external packages, never match internal files. Easy to filter: no `.` or `/` prefix.
- `index.ts` resolution: `import from './utils'` might resolve to `./utils.ts` OR `./utils/index.ts`. Need to check both.
- Dynamic imports (`import('./foo')`) — same as static for path resolution, but wrapped in a call expression. Not captured by current `extract_imports()`. Fall back.
- Re-exports (`export { foo } from './bar'`) — captured as `export_statement`, not `import_statement`. Would need to extend import extraction.

**Verdict**: **Works for relative imports.** Bare specifiers are trivially excluded. Path aliases need tsconfig parsing or fallback. Re-exports need minor extension.

## Approach

### Two-phase scan

1. **Lightweight import scan**: For each file in the repo, extract imports only (no function/call parsing). This is cheap — `extract_imports()` only walks top-level children.
2. **Filtered full parse**: Only files whose imports reference the changed file's module get full `extract_calls()` + `extract_functions()` parsing.

### Module path inference

A new function `infer_module_path(file_path, language) -> Option<ModulePath>` returns a language-specific module identifier. This is a pure function of the file path (plus `go.mod` content for Go).

### Import target extraction

A new function `extract_import_targets(imports: &[String], language) -> Vec<String>` normalizes the raw import strings from `extract_imports()` into module path strings that can be compared against inferred module paths.

For Rust: `use crate::foo::bar::Thing;` → `crate::foo::bar`
For Python: `from src.validation import check` → `src.validation`
For Go: `"github.com/foo/bar/internal/parser"` → `internal/parser`
For TS: `import { x } from './validation';` → resolve relative to importer

### Fallback triggers

Fall back to full scan (current behavior) when:
- Language has no import resolution support (Ruby, C, PHP, etc.)
- Dynamic imports detected
- Path alias detected (TS/JS `@/`, `~/`)
- Import resolution fails for any file (safety net)

### Same-directory inclusion

Files in the same directory as the changed file are always included in the scan, regardless of import analysis. This catches same-package callers that don't need explicit imports (Go same-package, Rust same-module `use super::`, Python relative imports).

## Performance expectations

- **Import-only scan**: ~1-2ms per file (vs ~7ms for full parse with calls+functions)
- **Typical filtering**: In a 500-file repo where a change touches 2 files, likely ~20-50 files import from those modules. Parse time drops from ~3.5s to ~0.5s.
- **Worst case**: Every file imports the changed module → same as brute force + overhead of import scan. Net ~10% slower. Acceptable because this case means every file is actually a potential caller.

## Decision

**Proceed with implementation** for Rust, Python, Go, and TypeScript/JavaScript. Fall back to full scan for all other languages. The approach is:

1. Infer module paths from file paths (language-specific, deterministic)
2. Extract import targets from import statements (reuse existing `extract_imports()`)
3. Filter the file scan to files that import from changed modules + same-directory files
4. Fall back to full scan when import resolution is ambiguous or unsupported

### Language support priority

| Language | Confidence | Notes |
|----------|-----------|-------|
| Go | High | Explicit import paths, no ambiguity |
| Rust | High | `crate::` paths map directly to files |
| Python | Medium | Direct imports reliable; relative imports need heuristics |
| TypeScript/JS | Medium | Relative imports reliable; path aliases need fallback |

## Consequences

- **Positive**: Fewer false positives, faster scan, better signal for blast radius scoring
- **Negative**: Added complexity in module path inference; edge cases in Python/TS may produce false negatives (missed callers)
- **Mitigation**: Fallback to full scan ensures no silent data loss. Response will indicate whether scoped or fallback scan was used.
