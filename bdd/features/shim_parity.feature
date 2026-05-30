# BDD bootstrap for the shim parity → sunset redirect hook epic (#328).
#
# Each scenario is tagged with the implementation issue that will make it
# GREEN. Until that issue lands, @not_implemented keeps the scenario excluded
# from CI. The first commit on each implementation branch removes
# @not_implemented from its targeted scenarios (the RED commit) before any
# production code is written.
#
# Tag conventions:
#   @ISSUE-322  -- Windows spawn+wait passthrough parity (exit code + I/O match)
#   @ISSUE-323  -- gh CLI interception via shim symlink
#   @ISSUE-324  -- git-prism shim install|uninstall|status subcommand
#   @ISSUE-325  -- auto-PATH setup UX during shim install
#   @ISSUE-326  -- redirect hook deprecation and eventual removal
#   @not_implemented -- applied to every scenario; CI excludes these.
#   @windows    -- scenarios that can only be genuinely RED on Windows;
#                  see the PLATFORM NOTE in the @ISSUE-322 Rule below.
#
# Step definitions shell out to the real compiled git-prism binary or to
# shim symlinks built in per-scenario tempdirs. None of them mock or stub —
# when the implementation does not yet exist the steps fail with assertion
# errors that document the contract being tested.

Feature: Shim parity with the redirect hook and first-class shim subcommand
  The PATH shim must be a strict superset of the redirect hook's coverage.
  That requires: (a) correct passthrough on all platforms, (b) gh CLI
  interception routed through git-prism, (c) a first-class shim subcommand
  replacing the --path-shim alias, (d) user-friendly auto-PATH setup that
  explains the required Claude Code restart, and (e) a clear deprecation
  path that names git-prism shim install as the replacement for the old
  hooks install workflow. ADR 0010 establishes the install+restart model
  that all auto-PATH scenarios depend on.

  # ===========================================================================
  # @ISSUE-322 — Windows shim passthrough parity
  #
  # PLATFORM NOTE: On unix, git passthrough via spawn+wait already works —
  # the existing shim uses execvp on unix. These scenarios assert that the
  # Windows code path (which cannot use execvp and must use spawn+wait)
  # produces identical observable behaviour: same exit code, same stdout,
  # same stderr. They are tagged @windows because the Windows-specific code
  # path under test is unreachable on linux/macOS; running them locally on
  # unix will PASS (spawn+wait also works there) rather than FAIL — meaning
  # they are NOT genuinely RED on unix. The Windows CI job (windows-latest)
  # is the authoritative RED gate for these scenarios. See the report
  # section for explicit honesty about this.
  # ===========================================================================

  Rule: Windows passthrough preserves real-git exit code and output

    @ISSUE-322 @windows
    Scenario: Successful git command exit code and stdout match real git on Windows
      Given a fixture git repository with two commits
      When the shim runs "git status" without agent env vars
      And the real git binary runs "git status" in the same repository
      Then the shim exit code matches the real git exit code
      And the shim stdout matches the real git stdout

    @ISSUE-322 @windows
    Scenario: Failed git command exit code and stderr match real git on Windows
      Given a fixture git repository with two commits
      When the shim runs "git not-a-real-subcommand" without agent env vars
      And the real git binary runs "git not-a-real-subcommand" in the same repository
      Then the shim exit code matches the real git exit code
      And the shim stderr matches the real git stderr

  # ===========================================================================
  # @ISSUE-323 — gh CLI interception
  #
  # When a symlink named "gh" points at the git-prism shim binary, the shim
  # must recognise argv[0] == "gh" and route pr diff calls through git-prism's
  # structured diff pipeline. All other gh subcommands pass through unchanged.
  #
  # All scenarios are hermetic: a stub "gh" script is placed on PATH ahead of
  # any real gh installation. The stub responds with fixture data so no network
  # access or GitHub authentication is required. The fixture git repo provides
  # the real commits whose SHAs the stub returns.
  # ===========================================================================

  Rule: gh pr diff is routed through git-prism structured output

    @ISSUE-323
    Scenario: gh pr diff produces structured JSON when invoked via the shim
      Given a fixture git repository with two commits
      And the shim is installed with a "gh" symlink
      And a stub gh binary is installed that knows the fixture PR SHAs
      When an agent runs "gh pr diff 1" via the shim with CLAUDECODE=1
      Then the exit code is 0
      And the output is valid JSON
      And the JSON output has a "files" array
      And each entry in the "files" array has a "change_scope" field

  Rule: Non-diff gh commands pass through to the real gh binary unchanged

    @ISSUE-323
    Scenario: gh repo view passes through with matching exit code and output
      Given the shim is installed with a "gh" symlink
      And a stub gh binary is installed for passthrough comparison
      When an agent runs "gh repo view --json name" via the shim with CLAUDECODE=1
      And the stub gh binary runs "gh repo view --json name" directly
      Then the shim exit code matches the real gh exit code
      And the shim stdout matches the real gh stdout

    @ISSUE-323
    Scenario: gh issue list passes through unchanged
      Given the shim is installed with a "gh" symlink
      And a stub gh binary is installed for passthrough comparison
      When an agent runs "gh issue list --limit 1" via the shim with CLAUDECODE=1
      And the stub gh binary runs "gh issue list --limit 1" directly
      Then the shim exit code matches the real gh exit code
      And the shim stdout matches the real gh stdout

  # ===========================================================================
  # @ISSUE-324 — git-prism shim subcommand
  #
  # The "hooks install --path-shim" alias is deprecated in favour of the
  # first-class "git-prism shim install|uninstall|status" subcommand.
  # The alias must still work for one release but print a deprecation warning.
  # ===========================================================================

  Rule: git-prism shim install creates the symlink via the new subcommand

    @ISSUE-324
    Scenario: shim install creates the git symlink in the shim directory
      Given an isolated HOME directory
      When I run "git-prism shim install" with the isolated HOME
      Then the exit code is 0
      And the path ".local/share/git-prism/bin/git" is a symlink under HOME

    @ISSUE-324
    Scenario: shim install is idempotent
      Given an isolated HOME directory
      When I run "git-prism shim install" with the isolated HOME
      And I run "git-prism shim install" with the isolated HOME again
      Then the exit code is 0
      And the path ".local/share/git-prism/bin/git" is a symlink under HOME

  Rule: git-prism shim status reports installation state

    @ISSUE-324
    Scenario: shim status reports active after install
      Given an isolated HOME directory
      When I run "git-prism shim install" with the isolated HOME
      And I run "git-prism shim status" with the isolated HOME
      Then the exit code is 0
      And the output contains "installed"
      And the output contains ".local/share/git-prism/bin"

    @ISSUE-324
    Scenario: shim status reports absent before install
      Given an isolated HOME directory
      When I run "git-prism shim status" with the isolated HOME
      Then the exit code is 0
      And the output contains "not installed"

  Rule: git-prism shim uninstall removes the symlink

    @ISSUE-324
    Scenario: shim uninstall removes the git symlink
      Given an isolated HOME directory
      When I run "git-prism shim install" with the isolated HOME
      And I run "git-prism shim uninstall" with the isolated HOME
      Then the exit code is 0
      And the path ".local/share/git-prism/bin/git" does not exist under HOME

  Rule: deprecated --path-shim alias still works but warns

    @ISSUE-324
    Scenario: hooks install --path-shim prints a deprecation warning to stderr
      Given an isolated HOME directory
      When I run "git-prism hooks install --path-shim" with the isolated HOME
      Then the exit code is 0
      And the stderr contains "deprecated"
      And the stderr contains "git-prism shim install"
      And the path ".local/share/git-prism/bin/git" is a symlink under HOME

  # ===========================================================================
  # @ISSUE-325 — auto-PATH setup UX
  #
  # Per ADR 0010: appending the export PATH line to the shell rc takes effect
  # only on the NEXT claude launch (the snapshot is frozen at launch time).
  # The install output MUST tell the user to restart Claude Code — this is not
  # optional wording. Scenarios reflect the three consent outcomes: accept,
  # decline, and already-in-PATH (no prompt needed).
  # ===========================================================================

  Rule: User consents to auto-PATH setup — rc file is updated idempotently

    @ISSUE-325
    Scenario: Accepting PATH setup appends the export line to the shell rc
      Given an isolated HOME directory
      And the shim directory is not in PATH
      And a shell rc file exists at ".zshrc" under HOME
      When I run "git-prism shim install" and consent to PATH setup with the isolated HOME
      Then the exit code is 0
      And the rc file ".zshrc" under HOME contains the shim directory export line
      And the output contains "restart"

    @ISSUE-325
    Scenario: Re-running install does not append the export line a second time
      Given an isolated HOME directory
      And the shim directory is not in PATH
      And a shell rc file exists at ".zshrc" under HOME
      When I run "git-prism shim install" and consent to PATH setup with the isolated HOME
      And I run "git-prism shim install" and consent to PATH setup with the isolated HOME again
      Then the rc file ".zshrc" under HOME contains the shim export line exactly once

  Rule: User declines auto-PATH setup — rc file is not modified

    @ISSUE-325
    Scenario: Declining PATH setup prints manual instructions and leaves rc unchanged
      Given an isolated HOME directory
      And the shim directory is not in PATH
      And a shell rc file exists at ".zshrc" under HOME
      When I run "git-prism shim install" and decline PATH setup with the isolated HOME
      Then the exit code is 0
      And the rc file ".zshrc" under HOME is unchanged
      And the output contains ".local/share/git-prism/bin"

  Rule: Shim directory already in PATH — no prompt, no rc modification

    @ISSUE-325
    Scenario: No PATH prompt when shim directory is already in PATH
      Given an isolated HOME directory
      And the shim directory is already in PATH
      And a shell rc file exists at ".zshrc" under HOME
      When I run "git-prism shim install" with the isolated HOME
      Then the exit code is 0
      And the rc file ".zshrc" under HOME is unchanged

  # ===========================================================================
  # @ISSUE-326 — redirect hook removal (hard removal, no deprecation phase)
  #
  # The redirect hook (bash_redirect_hook.py) is removed entirely in v0.9.0.
  # hooks install without --path-shim exits non-zero and names the replacement.
  # bash_redirect_hook.py is absent from the repository after removal.
  # ADR-0011 documents the rationale.
  # ===========================================================================

  Rule: hooks install without --path-shim is a hard error after redirect hook removal

    @ISSUE-326
    Scenario: hooks install exits non-zero and names git-prism shim install
      Given an isolated HOME with an empty .claude directory
      When I run "git-prism hooks install" with an isolated HOME
      Then the exit code is not 0
      And the stderr contains "git-prism shim install"

    @ISSUE-326
    Scenario: bash_redirect_hook.py is absent from the repository
      When I inspect the git-prism hooks directory
      Then "hooks/bash_redirect_hook.py" does not exist in the installation
