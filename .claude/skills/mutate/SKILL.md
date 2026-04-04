---
name: mutate
description: Run mutation testing with cargo-mutants. Use after writing tests to verify they actually catch regressions — survivors indicate weak assertions.
---

Run mutation testing on git-prism using `cargo-mutants`.

## Default: Targeted Run

If the user specifies files or modules, scope the run:

```bash
cargo mutants -- -f <file_or_pattern>
```

If no scope specified, run on the most recently changed files:

```bash
# Get files changed vs main
git diff --name-only origin/main...HEAD | grep '\.rs$'
# Then run mutants on those files
cargo mutants -- -f <each_file>
```

## Full Run

If the user says "full" or "all":

```bash
cargo mutants
```

This is slow on a full codebase — prefer targeted runs during development.

## Interpreting Results

- **Killed:** Test suite caught the mutation. Good.
- **Survived:** A mutation passed all tests — a test gap. Investigate and add a test.
- **Timeout:** Mutation caused an infinite loop. Usually fine — means tests do exercise that code path.
- **Unviable:** Mutation didn't compile. Ignored.

For each survivor, report:
1. Which function was mutated
2. What the mutation was (e.g., "replaced `>` with `>=`")
3. Which test should have caught it but didn't
4. Suggest a specific test to add

## $ARGUMENTS

If provided, use as the file/module scope: `cargo mutants -- -f $ARGUMENTS`
