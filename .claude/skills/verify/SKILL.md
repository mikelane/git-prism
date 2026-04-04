---
name: verify
description: Run the full lint, format check, and test suite. Use before committing or when checking if changes are valid.
---

Run the full verification suite for git-prism. Execute these commands in order, stopping on first failure:

```bash
cargo clippy -- -D warnings
cargo fmt --check
cargo test
```

Report results clearly: which step passed, which failed (if any), and the relevant error output.

If `cargo fmt --check` fails, offer to run `cargo fmt` to fix formatting automatically.

After all three pass, ask if the user wants to run mutation testing (`/mutate`) on the changed files. Don't run it automatically — it's slow.
