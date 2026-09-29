use super::{ProviderOutput, RunSpec};
use crate::model::TaskMode;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::process::Command;

pub fn command(spec: &RunSpec) -> Result<Command> {
    let has_key = std::env::var("ANTHROPIC_API_KEY")
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    build_command(spec, has_key)
}

fn build_command(spec: &RunSpec, has_key: bool) -> Result<Command> {
    if !has_key {
        bail!("Claude requires a nonempty ANTHROPIC_API_KEY for the Prodex API-backed integration");
    }
    if let Some(session) = &spec.session_id {
        uuid::Uuid::parse_str(session).context("Claude session ID must be a UUID")?;
    }
    let tools = match spec.mode {
        TaskMode::ReadOnly => "Read,Glob,Grep",
        TaskMode::Edit | TaskMode::EditInPlace => "Read,Glob,Grep,Edit,Write",
        TaskMode::InitializeRepository => "Read,Glob,Grep,Edit,Write,Bash",
    };
    let mut command = Command::new("claude");
    command.current_dir(&spec.cwd).args([
        "--bare",
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--strict-mcp-config",
        "--permission-mode",
        "dontAsk",
        "--permission-prompts",
        "none",
        "--tools",
        tools,
        "--allowedTools",
        if spec.mode == TaskMode::InitializeRepository {
            "Read,Glob,Grep,Edit,Write,Bash(git *)"
        } else {
            tools
        },
    ]);
    // Keep this adapter on its declared API-key path even when the host shell
    // also has an interactive subscription or cloud-provider configuration.
    for name in [
        "CLAUDE_CODE_OAUTH_TOKEN",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        command.env_remove(name);
    }
    if let Some(session) = &spec.session_id {
        command.args(["--resume", session]);
    }
    Ok(command)
}

pub fn parse_line(line: &str) -> Vec<ProviderOutput> {
    if line.trim().is_empty() {
        return vec![];
    }
    let value: Value = match serde_json::from_str::<Value>(line) {
        Ok(value) if value.is_object() => value,
        _ => return vec![ProviderOutput::Error("Invalid Claude JSON event".into())],
    };
    let mut events = Vec::new();
    if let Some(session) = value.get("session_id").and_then(Value::as_str) {
        if uuid::Uuid::parse_str(session).is_ok() {
            events.push(ProviderOutput::Session(session.to_owned()));
        } else {
            events.push(ProviderOutput::Error(
                "Invalid Claude session ID in event".into(),
            ));
        }
    }
    match value.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            if let Some(content) = value.pointer("/message/content").and_then(Value::as_array) {
                for block in content {
                    if block.get("type").and_then(Value::as_str) == Some("text")
                        && let Some(text) = block.get("text").and_then(Value::as_str)
                    {
                        events.push(ProviderOutput::Text(text.to_owned()));
                    }
                }
            }
        }
        Some("result") => {
            let success = value.get("subtype").and_then(Value::as_str) == Some("success")
                && value.get("is_error").and_then(Value::as_bool) == Some(false);
            let summary = value
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    value.get("errors").and_then(Value::as_array).map(|errors| {
                        errors
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .filter(|summary| !summary.is_empty())
                .unwrap_or_else(|| {
                    if success {
                        "Claude completed"
                    } else {
                        "Claude run failed"
                    }
                    .into()
                });
            if let Some(usd) = value.get("total_cost_usd").and_then(Value::as_f64)
                && usd.is_finite()
                && usd >= 0.0
            {
                // Provider total is cumulative for a resumed conversation.
                events.push(ProviderOutput::Usage { usd });
            }
            events.push(ProviderOutput::Completed { success, summary });
        }
        Some("system")
            if value.get("subtype").and_then(Value::as_str) == Some("permission_denied") =>
        {
            events.push(ProviderOutput::Text(
                "Claude tool permission denied by managed policy".into(),
            ));
        }
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;

    const SESSION: &str = "8acdecc2-1a61-47e0-b364-b3772123ef32";

    fn spec(mode: TaskMode) -> RunSpec {
        RunSpec {
            provider: Provider::Claude,
            cwd: "/tmp/project with spaces".into(),
            prompt: "a prompt; $(not a shell command)".into(),
            mode,
            session_id: None,
        }
    }

    #[test]
    fn authentication_and_resume_fail_closed() {
        assert!(build_command(&spec(TaskMode::ReadOnly), false).is_err());
        let mut run = spec(TaskMode::Edit);
        run.session_id = Some("--dangerously-skip-permissions".into());
        assert!(build_command(&run, true).is_err());
    }

    #[test]
    fn command_keeps_prompt_off_argv_and_tools_bounded() {
        for (mode, expected) in [
            (TaskMode::ReadOnly, "Read,Glob,Grep"),
            (TaskMode::Edit, "Read,Glob,Grep,Edit,Write"),
        ] {
            let mut run = spec(mode);
            run.session_id = Some(SESSION.into());
            let command = build_command(&run, true).unwrap();
            let command = command.as_std();
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect();
            assert!(args.windows(2).any(|pair| pair == ["--tools", expected]));
            assert!(args.windows(2).any(|pair| pair == ["--resume", SESSION]));
            assert!(args.contains(&"--restricted"));
            assert!(!args.contains(&run.prompt.as_str()));
            assert_eq!(command.get_current_dir(), Some(run.cwd.as_path()));
        }
    }

    #[test]
    fn parses_session_and_only_assistant_text_blocks() {
        let init = format!(
            r#"{{"type":"system","subtype":"init","session_id":"{SESSION}","capabilities":["future"]}}"#
        );
        assert_eq!(
            parse_line(&init),
            vec![ProviderOutput::Session(SESSION.into())]
        );
        assert_eq!(
            parse_line(
                r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"Done"},{"type":"tool_use","name":"Read"}]}}"#
            ),
            vec![ProviderOutput::Text("Done".into())]
        );
    }

    #[test]
    fn git_setup_allows_git_shell_without_enabling_unrestricted_shell() {
        let spec = RunSpec {
            provider: crate::model::Provider::Claude,
            cwd: "/tmp/project".into(),
            prompt: "setup".into(),
            mode: TaskMode::InitializeRepository,
            session_id: None,
        };
        let cmd = build_command(&spec, true).unwrap();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert!(
            args.windows(2)
                .any(|a| a == ["--allowedTools", "Read,Glob,Grep,Edit,Write,Bash(git *)"])
        );
        assert!(
            args.windows(2)
                .any(|a| a == ["--permission-mode", "dontAsk"])
        );
    }

    #[test]
    fn result_error_is_not_success_even_with_success_subtype() {
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"success","is_error":true,"result":"Authentication failed"}"#
            ),
            vec![ProviderOutput::Completed {
                success: false,
                summary: "Authentication failed".into()
            }]
        );
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"error_max_turns","is_error":true,"errors":["Turn limit"]}"#
            ),
            vec![ProviderOutput::Completed {
                success: false,
                summary: "Turn limit".into()
            }]
        );
    }

    #[test]
    fn reports_cost_and_completion_without_assuming_unknown_events_failed() {
        assert_eq!(
            parse_line(
                r#"{"type":"result","subtype":"success","is_error":false,"result":"Finished","total_cost_usd":0.12}"#
            ),
            vec![
                ProviderOutput::Usage { usd: 0.12 },
                ProviderOutput::Completed {
                    success: true,
                    summary: "Finished".into()
                }
            ]
        );
        assert!(parse_line(r#"{"type":"future_extension","data":{}}"#).is_empty());
        assert!(parse_line("  ").is_empty());
        assert!(matches!(
            parse_line("{truncated").as_slice(),
            [ProviderOutput::Error(_)]
        ));
        assert!(matches!(
            parse_line(r#"{"type":"result"}"#).as_slice(),
            [ProviderOutput::Completed { success: false, .. }]
        ));
    }
}
