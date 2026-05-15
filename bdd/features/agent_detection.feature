@ISSUE-278
Feature: Agent detection via environment variables

  Background:
    Given the git-prism binary is available

  Scenario: Detect Claude Code via CLAUDECODE env var
    When I run agent-detect with env vars "CLAUDECODE=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "ClaudeCode"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Cursor via CURSOR_AGENT env var
    When I run agent-detect with env vars "CURSOR_AGENT=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Cursor"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Gemini CLI via GEMINI_CLI env var
    When I run agent-detect with env vars "GEMINI_CLI=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Gemini"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Codex via CODEX_SANDBOX env var
    When I run agent-detect with env vars "CODEX_SANDBOX=seatbelt"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Codex"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Cline via CLINE_ACTIVE env var
    When I run agent-detect with env vars "CLINE_ACTIVE=true"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Cline"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Augment via AUGMENT_AGENT env var
    When I run agent-detect with env vars "AUGMENT_AGENT=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Augment"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect OpenCode via OPENCODE_CLIENT env var
    When I run agent-detect with env vars "OPENCODE_CLIENT=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "OpenCode"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect TRAE AI via TRAE_AI_SHELL_ID env var
    When I run agent-detect with env vars "TRAE_AI_SHELL_ID=session-123"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Trae"
    And the JSON field "signal" is "ToolSpecific"

  Scenario: Detect Goose via AGENT env var
    When I run agent-detect with env vars "AGENT=goose"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Goose"
    And the JSON field "signal" is "Agent"

  Scenario: Detect Amp via AGENT env var
    When I run agent-detect with env vars "AGENT=amp"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "Amp"
    And the JSON field "signal" is "Agent"

  Scenario: AGENT=1 is not honored to avoid collision with ssh-agent
    When I run agent-detect with env vars "AGENT=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is null
    And the JSON field "signal" is null

  Scenario: Prefer AI_AGENT over tool-specific markers when both are set
    When I run agent-detect with env vars "AI_AGENT=claude-code_2-1-141_agent,CLAUDECODE=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "signal" is "AiAgent"

  Scenario: Parse Vercel AI_AGENT format and surface the tool name
    When I run agent-detect with env vars "AI_AGENT=claude-code_2-1-141_agent"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is "ClaudeCode"
    And the JSON field "signal" is "AiAgent"

  Scenario: AI_AGENT with unknown tool produces Unknown agent
    When I run agent-detect with env vars "AI_AGENT=sometool_1-0_agent"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "signal" is "AiAgent"
    And the JSON field "agent" is "Unknown"

  Scenario: CI env var overrides agent marker
    When I run agent-detect with env vars "CI=true,CLAUDECODE=1"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is null
    And the JSON field "signal" is null

  Scenario: Empty environment returns null
    When I run agent-detect with empty environment
    Then the exit code is 0
    And the output is valid JSON
    And the JSON field "agent" is null
    And the JSON field "signal" is null
