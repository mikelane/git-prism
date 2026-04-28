"""Pytest configuration for `hooks/` tests.

The bundled redirect hook script invokes the Python helper as
`python3 -m bash_redirect_hook` after `cd`-ing into `hooks/`, so the
module loads as a top-level (no `hooks` package). Mirror that here by
prepending `hooks/` to `sys.path` so `from bash_redirect_hook import ...`
resolves the same way in the pytest run.
"""

from __future__ import annotations

import sys
from pathlib import Path

_HOOKS_DIR = Path(__file__).resolve().parent.parent
if str(_HOOKS_DIR) not in sys.path:
    sys.path.insert(0, str(_HOOKS_DIR))
