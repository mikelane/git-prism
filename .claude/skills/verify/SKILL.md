---
name: verify
description: Run the full lint, format check, and test suite. Use before committing or when checking if changes are valid.
---

Run the full verification suite for git-prism. Execute these commands in order, stopping on first failure:

```bash
cargo clippy -- -D warnings
cargo fmt --check
cargo test
cargo build --release
python -m behave bdd/features/ --no-capture --tags="not @crates_io" --tags="not @not_implemented"
```

Report results clearly: which step passed, which failed (if any), and the relevant error output.

If `cargo fmt --check` fails, offer to run `cargo fmt` to fix formatting automatically.

If `python -m behave` fails with a missing module error, install BDD dependencies first:
```bash
pip install behave grpcio opentelemetry-proto protobuf
```

After all five pass, ask if the user wants to run mutation testing (`/mutate`) on the changed files. Don't run it automatically — it's slow.
