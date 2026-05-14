/// Agent detection module.
///
/// Answers "is the current process running on behalf of an AI coding agent?"
/// using only environment variables. No process inspection, no terminal-program
/// sniffing — those have too-high false-positive rates and are out of scope.
///
/// Call `detect_calling_agent` with an `EnvSource` implementation. The
/// production path uses `StdEnvSource`; tests inject a `HashMap`-backed stub.
/// **Do not call `std::env::var` inside the detection logic itself** — only
/// through `EnvSource`. That contract is what keeps tests hermetic.
use schemars::JsonSchema;
use serde::Serialize;

/// Abstracts environment-variable lookup so detection logic is hermetic in tests.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

/// Production `EnvSource` that reads from the real process environment.
pub struct StdEnvSource;

impl EnvSource for StdEnvSource {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Which agent was detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub enum AgentName {
    ClaudeCode,
    Cursor,
    Gemini,
    Codex,
    Cline,
    Augment,
    OpenCode,
    Trae,
    Goose,
    Amp,
    Unknown { tool: String },
}

/// Which detection signal fired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub enum DetectionSignal {
    /// Matched via the cross-tool `AI_AGENT=<tool>_<version>_agent` convention.
    AiAgent,
    /// Matched via `AGENT=<name>` from the agents.md proposal.
    Agent,
    /// Matched via a tool-specific env var (e.g. `CLAUDECODE`, `CURSOR_AGENT`).
    ToolSpecific,
}

/// The result of a successful agent detection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DetectedAgent {
    /// Which agent was identified.
    pub name: AgentName,
    /// Which detection signal fired.
    pub signal: DetectionSignal,
    /// The raw env var value that matched, for debugging.
    pub raw_value: String,
}

/// Detect the calling agent from environment variables.
///
/// Returns `None` when:
/// - `CI` is set (CI tooling wins over all agent signals), or
/// - no known agent markers are present.
///
/// Detection priority order (per research doc §5):
/// 1. `AI_AGENT` non-empty
/// 2. `AGENT` with value in the allowlist (`goose`, `amp`)
/// 3. Tool-specific markers
/// 4. `CI` set → `None` regardless of the above
pub fn detect_calling_agent(env: &dyn EnvSource) -> Option<DetectedAgent> {
    // CI wins over all agent signals — check first
    if env.get("CI").is_some() {
        return None;
    }

    // Priority 1: AI_AGENT (Vercel cross-tool convention)
    if let Some(value) = env.get("AI_AGENT").filter(|v| !v.is_empty()) {
        let name = parse_ai_agent_value(&value);
        return Some(DetectedAgent {
            name,
            signal: DetectionSignal::AiAgent,
            raw_value: value,
        });
    }

    // Priority 2: AGENT with allowlisted value
    if let Some(value) = env.get("AGENT") {
        let agent_name = match value.as_str() {
            "goose" => Some(AgentName::Goose),
            "amp" => Some(AgentName::Amp),
            _ => None,
        };
        if let Some(name) = agent_name {
            return Some(DetectedAgent {
                name,
                signal: DetectionSignal::Agent,
                raw_value: value,
            });
        }
    }

    // Priority 3: Tool-specific markers
    let tool_specific: &[(&str, AgentName)] = &[
        ("CLAUDECODE", AgentName::ClaudeCode),
        ("CURSOR_AGENT", AgentName::Cursor),
        ("GEMINI_CLI", AgentName::Gemini),
        ("CODEX_SANDBOX", AgentName::Codex),
        ("CLINE_ACTIVE", AgentName::Cline),
        ("AUGMENT_AGENT", AgentName::Augment),
        ("OPENCODE_CLIENT", AgentName::OpenCode),
        ("TRAE_AI_SHELL_ID", AgentName::Trae),
    ];

    for (var, name) in tool_specific {
        if let Some(value) = env.get(var).filter(|v| !v.is_empty()) {
            return Some(DetectedAgent {
                name: name.clone(),
                signal: DetectionSignal::ToolSpecific,
                raw_value: value,
            });
        }
    }

    None
}

/// Parse the `AI_AGENT` value.
///
/// Expected format: `<tool>_<version>_agent`, e.g. `claude-code_2-1-141_agent`.
/// The tool segment maps to a known `AgentName`. Unknown tools produce
/// `AgentName::Unknown { tool }`.
fn parse_ai_agent_value(value: &str) -> AgentName {
    // Strip trailing `_agent` suffix if present
    let tool_version = value.strip_suffix("_agent").unwrap_or(value);

    // Collect underscore-delimited segments until a version segment (starts with a
    // digit) is found. e.g. "claude-code_2-1-141" splits as ["claude-code", "2-1-141"];
    // the version segment is dropped, leaving ["claude-code"] joined with "-".
    // Multi-word tool names (e.g. "open_code_1-0") would join as "open-code".
    let tool = tool_version
        .split('_')
        .take_while(|seg| !seg.starts_with(|c: char| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join("-");

    match tool.as_str() {
        "claude-code" => AgentName::ClaudeCode,
        "cursor" => AgentName::Cursor,
        "gemini-cli" => AgentName::Gemini,
        "codex" => AgentName::Codex,
        "cline" => AgentName::Cline,
        "augment" => AgentName::Augment,
        "opencode" => AgentName::OpenCode,
        "trae" => AgentName::Trae,
        "goose" => AgentName::Goose,
        "amp" => AgentName::Amp,
        other => AgentName::Unknown {
            tool: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct StubEnv(HashMap<&'static str, &'static str>);

    impl EnvSource for StubEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).map(|v| v.to_string())
        }
    }

    fn env(pairs: &[(&'static str, &'static str)]) -> StubEnv {
        StubEnv(pairs.iter().copied().collect())
    }

    fn empty() -> StubEnv {
        StubEnv(HashMap::new())
    }

    // --- CLAUDECODE ---

    #[test]
    fn it_detects_claude_code_via_claudecode_var() {
        let result = detect_calling_agent(&env(&[("CLAUDECODE", "1")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::ClaudeCode,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "1".to_string(),
            })
        );
    }

    // --- CURSOR_AGENT ---

    #[test]
    fn it_detects_cursor_via_cursor_agent_var() {
        let result = detect_calling_agent(&env(&[("CURSOR_AGENT", "1")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Cursor,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "1".to_string(),
            })
        );
    }

    // --- GEMINI_CLI ---

    #[test]
    fn it_detects_gemini_via_gemini_cli_var() {
        let result = detect_calling_agent(&env(&[("GEMINI_CLI", "1")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Gemini,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "1".to_string(),
            })
        );
    }

    // --- CODEX_SANDBOX ---

    #[test]
    fn it_detects_codex_via_codex_sandbox_var() {
        let result = detect_calling_agent(&env(&[("CODEX_SANDBOX", "seatbelt")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Codex,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "seatbelt".to_string(),
            })
        );
    }

    // --- CLINE_ACTIVE ---

    #[test]
    fn it_detects_cline_via_cline_active_var() {
        let result = detect_calling_agent(&env(&[("CLINE_ACTIVE", "true")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Cline,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "true".to_string(),
            })
        );
    }

    // --- AUGMENT_AGENT ---

    #[test]
    fn it_detects_augment_via_augment_agent_var() {
        let result = detect_calling_agent(&env(&[("AUGMENT_AGENT", "1")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Augment,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "1".to_string(),
            })
        );
    }

    // --- OPENCODE_CLIENT ---

    #[test]
    fn it_detects_opencode_via_opencode_client_var() {
        let result = detect_calling_agent(&env(&[("OPENCODE_CLIENT", "1")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::OpenCode,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "1".to_string(),
            })
        );
    }

    // --- TRAE_AI_SHELL_ID ---

    #[test]
    fn it_detects_trae_via_trae_ai_shell_id_var() {
        let result = detect_calling_agent(&env(&[("TRAE_AI_SHELL_ID", "session-123")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Trae,
                signal: DetectionSignal::ToolSpecific,
                raw_value: "session-123".to_string(),
            })
        );
    }

    // --- AGENT allowlist ---

    #[test]
    fn it_detects_goose_via_agent_var() {
        let result = detect_calling_agent(&env(&[("AGENT", "goose")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Goose,
                signal: DetectionSignal::Agent,
                raw_value: "goose".to_string(),
            })
        );
    }

    #[test]
    fn it_detects_amp_via_agent_var() {
        let result = detect_calling_agent(&env(&[("AGENT", "amp")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::Amp,
                signal: DetectionSignal::Agent,
                raw_value: "amp".to_string(),
            })
        );
    }

    #[test]
    fn it_ignores_agent_var_with_non_allowlisted_value() {
        let result = detect_calling_agent(&env(&[("AGENT", "1")]));
        assert_eq!(result, None);
    }

    // --- AI_AGENT ---

    #[test]
    fn it_detects_claude_code_via_ai_agent_var() {
        let result = detect_calling_agent(&env(&[("AI_AGENT", "claude-code_2-1-141_agent")]));
        assert_eq!(
            result,
            Some(DetectedAgent {
                name: AgentName::ClaudeCode,
                signal: DetectionSignal::AiAgent,
                raw_value: "claude-code_2-1-141_agent".to_string(),
            })
        );
    }

    #[test]
    fn it_prefers_ai_agent_over_tool_specific_markers() {
        let result = detect_calling_agent(&env(&[
            ("AI_AGENT", "claude-code_2-1-141_agent"),
            ("CLAUDECODE", "1"),
        ]));
        let detected = result.expect("should detect an agent");
        assert_eq!(detected.signal, DetectionSignal::AiAgent);
    }

    #[test]
    fn it_produces_unknown_agent_for_unrecognized_ai_agent_tool() {
        let result = detect_calling_agent(&env(&[("AI_AGENT", "sometool_1-0_agent")]));
        let detected = result.expect("should detect an agent");
        assert_eq!(detected.signal, DetectionSignal::AiAgent);
        assert!(matches!(detected.name, AgentName::Unknown { .. }));
    }

    // --- CI override ---

    #[test]
    fn it_returns_none_when_ci_is_set_alongside_claudecode() {
        let result = detect_calling_agent(&env(&[("CI", "true"), ("CLAUDECODE", "1")]));
        assert_eq!(result, None);
    }

    #[test]
    fn it_returns_none_when_ci_is_set_alongside_ai_agent() {
        let result = detect_calling_agent(&env(&[
            ("CI", "true"),
            ("AI_AGENT", "claude-code_2-1-141_agent"),
        ]));
        assert_eq!(result, None);
    }

    // --- empty environment ---

    #[test]
    fn it_returns_none_in_empty_environment() {
        let result = detect_calling_agent(&empty());
        assert_eq!(result, None);
    }

    // --- triangulation: edge cases ---

    #[test]
    fn it_ignores_empty_claudecode_var() {
        let result = detect_calling_agent(&env(&[("CLAUDECODE", "")]));
        assert_eq!(result, None);
    }

    #[test]
    fn it_ignores_empty_ai_agent_var() {
        let result = detect_calling_agent(&env(&[("AI_AGENT", "")]));
        assert_eq!(result, None);
    }

    #[test]
    fn it_ignores_agent_var_with_uppercase_goose() {
        // Allowlist match is case-sensitive
        let result = detect_calling_agent(&env(&[("AGENT", "GOOSE")]));
        assert_eq!(result, None);
    }

    #[test]
    fn it_detects_ai_agent_without_agent_suffix() {
        // Value without _agent suffix still works — tool is parsed from full value
        let result = detect_calling_agent(&env(&[("AI_AGENT", "claude-code")]));
        let detected = result.expect("should detect an agent");
        assert_eq!(detected.name, AgentName::ClaudeCode);
        assert_eq!(detected.signal, DetectionSignal::AiAgent);
    }

    // --- snapshot tests for JSON serialization ---

    #[test]
    fn it_serializes_detected_agent_to_json() {
        let detected = DetectedAgent {
            name: AgentName::ClaudeCode,
            signal: DetectionSignal::ToolSpecific,
            raw_value: "1".to_string(),
        };
        insta::assert_json_snapshot!(detected);
    }

    #[test]
    fn it_serializes_unknown_agent_to_json() {
        let detected = DetectedAgent {
            name: AgentName::Unknown {
                tool: "sometool".to_string(),
            },
            signal: DetectionSignal::AiAgent,
            raw_value: "sometool_1-0_agent".to_string(),
        };
        insta::assert_json_snapshot!(detected);
    }
}
