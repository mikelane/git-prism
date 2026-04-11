"""Step definitions for wrapper-pattern extraction scenarios.

Fixtures create repos with exported TypeScript functions, decorated Python
functions, and C++ extern "C" blocks. Assertion steps validate that
functions inside these wrapper nodes appear in manifest output.
"""

from __future__ import annotations

from behave import given
from behave.runner import Context

from repo_setup_steps import _commit, _init_repo, _write_file


# ---------- TypeScript: export function ----------

TS_EXPORT_INITIAL = """\
export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"""

TS_EXPORT_MODIFIED = """\
export function greet(name: string): string {
    return `Hello, ${name}! Welcome back.`;
}
"""


@given("a git repository with a TypeScript exported function change")
def step_repo_ts_export_function(context: Context) -> None:
    """Create a repo where an `export function` is modified between commits."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.ts", TS_EXPORT_INITIAL)
    _commit(repo_dir, "initial export function", ["lib.ts"])
    _write_file(repo_dir, "lib.ts", TS_EXPORT_MODIFIED)
    _commit(repo_dir, "modify exported function", ["lib.ts"])


# ---------- TypeScript: export default function ----------

TS_DEFAULT_INITIAL = """\
export default function handler(req: any): any {
    return { status: 200 };
}
"""

TS_DEFAULT_MODIFIED = """\
export default function handler(req: any): any {
    return { status: 200, data: req.body };
}
"""


@given("a git repository with a TypeScript export-default function change")
def step_repo_ts_export_default(context: Context) -> None:
    """Create a repo where an `export default function` is modified."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "handler.ts", TS_DEFAULT_INITIAL)
    _commit(repo_dir, "initial export default", ["handler.ts"])
    _write_file(repo_dir, "handler.ts", TS_DEFAULT_MODIFIED)
    _commit(repo_dir, "modify default handler", ["handler.ts"])


# ---------- TypeScript: export class ----------

TS_CLASS_INITIAL = """\
export class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }
}
"""

TS_CLASS_MODIFIED = """\
export class Calculator {
    add(a: number, b: number): number {
        return a + b;
    }

    multiply(a: number, b: number): number {
        return a * b;
    }
}
"""


@given("a git repository with a TypeScript exported class change")
def step_repo_ts_export_class(context: Context) -> None:
    """Create a repo where an `export class` with methods is modified."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "calculator.ts", TS_CLASS_INITIAL)
    _commit(repo_dir, "initial export class", ["calculator.ts"])
    _write_file(repo_dir, "calculator.ts", TS_CLASS_MODIFIED)
    _commit(repo_dir, "add multiply method to exported class", ["calculator.ts"])


# ---------- Python: @decorator def ----------

PY_DECORATED_INITIAL = """\
from flask import Flask

app = Flask(__name__)

@app.route("/")
def index():
    return "Hello, World!"
"""

PY_DECORATED_MODIFIED = """\
from flask import Flask

app = Flask(__name__)

@app.route("/")
def index():
    return "Hello, World! Updated."
"""


@given("a git repository with a Python decorated function change")
def step_repo_py_decorated(context: Context) -> None:
    """Create a repo where a @decorated function is modified."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "app.py", PY_DECORATED_INITIAL)
    _commit(repo_dir, "initial decorated function", ["app.py"])
    _write_file(repo_dir, "app.py", PY_DECORATED_MODIFIED)
    _commit(repo_dir, "modify decorated function", ["app.py"])


# ---------- Python: stacked decorators ----------

PY_STACKED_INITIAL = """\
from flask import Flask

app = Flask(__name__)

@app.route("/admin")
@app.route("/admin/")
def admin_page():
    return "Admin panel"
"""

PY_STACKED_MODIFIED = """\
from flask import Flask

app = Flask(__name__)

@app.route("/admin")
@app.route("/admin/")
def admin_page():
    return "Admin panel v2"
"""


@given("a git repository with a Python stacked-decorator function change")
def step_repo_py_stacked_decorators(context: Context) -> None:
    """Create a repo where a function with stacked decorators is modified."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "app.py", PY_STACKED_INITIAL)
    _commit(repo_dir, "initial stacked decorators", ["app.py"])
    _write_file(repo_dir, "app.py", PY_STACKED_MODIFIED)
    _commit(repo_dir, "modify stacked-decorator function", ["app.py"])


# ---------- C++: extern "C" ----------

CPP_EXTERN_C_INITIAL = """\
#include <cstdio>

extern "C" {

void ffi_init() {
    printf("init\\n");
}

void ffi_cleanup() {
    printf("cleanup\\n");
}

}
"""

CPP_EXTERN_C_MODIFIED = """\
#include <cstdio>

extern "C" {

void ffi_init() {
    printf("initialized v2\\n");
}

void ffi_cleanup() {
    printf("cleanup\\n");
}

}
"""


@given("a git repository with a C++ extern-C function change")
def step_repo_cpp_extern_c(context: Context) -> None:
    """Create a repo where a function inside extern "C" is modified."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "ffi.cpp", CPP_EXTERN_C_INITIAL)
    _commit(repo_dir, "initial extern C block", ["ffi.cpp"])
    _write_file(repo_dir, "ffi.cpp", CPP_EXTERN_C_MODIFIED)
    _commit(repo_dir, "modify ffi_init in extern C", ["ffi.cpp"])


# ---------- TypeScript: export function context fixture ----------

TS_CONTEXT_LIB_INITIAL = """\
export function compute(x: number): number {
    return x + 1;
}
"""

TS_CONTEXT_LIB_MODIFIED = """\
export function compute(x: number): number {
    return x * 2 + 1;
}
"""

TS_CONTEXT_CALLER = """\
import { compute } from './lib';

function main() {
    const result = compute(42);
    console.log(result);
}
"""


@given("a git repository with a TypeScript exported function context fixture")
def step_repo_ts_export_context(context: Context) -> None:
    """Create a TS repo with export function + a caller file."""
    repo_dir = _init_repo(context)
    _write_file(repo_dir, "lib.ts", TS_CONTEXT_LIB_INITIAL)
    _write_file(repo_dir, "main.ts", TS_CONTEXT_CALLER)
    _commit(repo_dir, "initial ts export", ["lib.ts", "main.ts"])
    _write_file(repo_dir, "lib.ts", TS_CONTEXT_LIB_MODIFIED)
    _commit(repo_dir, "modify exported compute", ["lib.ts"])
