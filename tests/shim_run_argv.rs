#![cfg(unix)]

//! `shim-run` must hand real git exactly the arguments the shell function
//! received, including arguments that are not valid UTF-8.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::Command;

#[test]
fn it_passes_non_utf8_arguments_through_to_real_git() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    // `git check-ref-format <name>` exits 0 for a valid ref name and prints
    // usage (exit 129) when the name argument is missing.
    let invalid_utf8_ref = OsStr::from_bytes(b"refs/heads/caf\xe9");

    let status = Command::new(bin)
        .args(["shim-run", "git", "check-ref-format"])
        .arg(invalid_utf8_ref)
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .env_remove("GIT_PRISM_DISABLE")
        .status()
        .unwrap();

    assert_eq!(
        status.code(),
        Some(0),
        "a non-UTF-8 argument must reach real git, not be silently dropped"
    );
}
