# BDD Acceptance Tests

Cross-language BDD tests for git-prism using Python + [behave](https://behave.readthedocs.io/).
Production code is Rust; these acceptance tests exercise the compiled binary from a separate language
to validate end-to-end behavior.

## Prerequisites

- Python 3.10+
- A built `git-prism` binary (`cargo build --release` from the project root)

## Setup

```bash
cd bdd
python3 -m venv .venv
source .venv/bin/activate
pip install -r requirements.txt
```

Or with `uv`:

```bash
cd bdd
uv venv
source .venv/bin/activate
uv pip install -r requirements.txt
```

## Running Tests

```bash
cd bdd
behave
```

Run a specific feature:

```bash
behave features/cli_version.feature
```

Run by tag (matching a GitHub issue):

```bash
behave --tags=@ISSUE-15
```

## Structure

```
bdd/
  features/          Gherkin .feature files
  steps/             Python step definitions
  environment.py     behave hooks (build binary, create/cleanup test repos)
  requirements.txt   Python dependencies
```
