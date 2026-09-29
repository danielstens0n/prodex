use super::{ProviderOutput, RunSpec};
use crate::model::TaskMode;
use serde_json::Value;
use tokio::process::Command;

/// A supervised, persistent CLI session. The service owns pipes and process lifetime.
pub fn command(spec: &RunSpec) -> anyhow::Result<Command> {
    anyhow::ensure!(spec.cwd.is_absolute(), "Codex cwd must be absolute");
    if let Some(id) = &spec.session_id {
        anyhow::ensure!(
            !id.trim().is_empty() && !id.starts_with('-'),
            "invalid Codex session ID"
        );
    }
    let mut command = Command::new("codex");
    command.current_dir(&spec.cwd).args([
        "--no-daemon",
        "--ask-for-approval",
        "never",
        "exec",
        "--ignore-user-config",
        "--ignore-rules",
        "--json",
    ]);
    if spec.mode == TaskMode::InitializeRepository {
        command.args([
            "-c",
            "default_permissions=\"prodex_git_setup\"",
            "-c",
            r#"permissions.prodex_git_setup={extends=":workspace",filesystem={":workspace_roots"={".git"="write"}}}"#,
            "--skip-git-repo-check",
        ]);
    } else {
        command.args([
            "--sandbox",
            if spec.mode == TaskMode::ReadOnly {
                "read-only"
            } else {
                "workspace-write"
            },
        ]);
        if matches!(spec.mode, TaskMode::ReadOnly | TaskMode::EditInPlace) {
            command.arg("--skip-git-repo-check");
        }
    }
    if let Some(id) = &spec.session_id {
        command.args(["resume", id]);
    }
    command.arg("-");
    Ok(command)
}

fn error_message(value: &Value) -> String {
    value
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
        .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
        .unwrap_or("Codex reported an error without a message")
        .to_owned()
}

/// Normalize `codex exec --json` JSONL, not App Server JSON-RPC notifications.
pub fn parse_line(line: &str) -> Vec<ProviderOutput> {
    if line.trim().is_empty() {
        return vec![];
    }
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return vec![ProviderOutput::Error("Malformed Codex JSON event".into())],
    };
    match value.get("type").and_then(Value::as_str) {
        Some("thread.started") => match value.get("thread_id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => vec![ProviderOutput::Session(id.to_owned())],
            _ => vec![ProviderOutput::Error(
                "Codex thread.started omitted thread_id".into(),
            )],
        },
        Some("item.completed") => {
            let item = &value["item"];
            match item.get("type").and_then(Value::as_str) {
                Some("agent_message") => match item.get("text").and_then(Value::as_str) {
                    Some(text) => vec![ProviderOutput::Text(text.to_owned())],
                    None => vec![ProviderOutput::Error(
                        "Codex agent_message omitted text".into(),
                    )],
                },
                Some("error") => vec![ProviderOutput::Error(error_message(item))],
                _ => vec![],
            }
        }
        Some("turn.completed") => vec![ProviderOutput::Completed {
            success: true,
            // Preserve accumulated agent text in the service; token counts aren't USD.
            summary: String::new(),
        }],
        Some("turn.failed") => vec![ProviderOutput::Completed {
            success: false,
            summary: error_message(&value),
        }],
        Some("error") => vec![ProviderOutput::Error(error_message(&value))],
        Some(_) => vec![], // Forward-compatible progress events.
        None => vec![ProviderOutput::Error("Codex event omitted type".into())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;

    #[test]
    fn normalizes_session_and_final_text_without_billing_guess() {
        assert_eq!(
            parse_line(r#"{"type":"thread.started","thread_id":"thread-1"}"#),
            vec![ProviderOutput::Session("thread-1".into())]
        );
        assert_eq!(
            parse_line(
                r#"{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"Tests passed."}}"#
            ),
            vec![ProviderOutput::Text("Tests passed.".into())]
        );
        assert_eq!(
            parse_line(
                r#"{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":20}}"#
            ),
            vec![ProviderOutput::Completed {
                success: true,
                summary: String::new()
            }]
        );
    }

    #[test]
    fn failures_and_malformed_input_cannot_be_success() {
        assert_eq!(
            parse_line(r#"{"type":"turn.failed","error":{"message":"quota exceeded"}}"#),
            vec![ProviderOutput::Completed {
                success: false,
                summary: "quota exceeded".into()
            }]
        );
        for input in [
            "not json",
            "{}",
            r#"{"type":"thread.started"}"#,
            r#"{"type":"error","message":"connection lost"}"#,
        ] {
            assert!(matches!(
                parse_line(input).as_slice(),
                [ProviderOutput::Error(_)]
            ));
        }
        assert!(parse_line(r#"{"type":"future.progress"}"#).is_empty());
        assert!(parse_line(" ").is_empty());
    }

    #[test]
    fn main_folder_coding_remains_sandboxed_and_can_run_without_git() {
        let spec = RunSpec {
            provider: Provider::Codex,
            cwd: "/tmp/project".into(),
            prompt: "code".into(),
            mode: TaskMode::EditInPlace,
            session_id: None,
        };
        let cmd = command(&spec).unwrap();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert!(
            args.windows(2)
                .any(|a| a == ["--sandbox", "workspace-write"])
        );
        assert!(args.iter().any(|a| a == "--skip-git-repo-check"));
        assert!(!args.iter().any(|a| a.contains("prodex_git_setup")));
    }

    #[test]
    fn setup_grants_only_scoped_git_metadata_access() {
        let spec = RunSpec {
            provider: Provider::Codex,
            cwd: "/tmp/project".into(),
            prompt: "setup".into(),
            mode: TaskMode::InitializeRepository,
            session_id: None,
        };
        let cmd = command(&spec).unwrap();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect();
        assert!(args.iter().any(|a| a == "--skip-git-repo-check"));
        assert!(
            args.iter()
                .any(|a| a.contains(r#"filesystem={":workspace_roots"={".git"="write"}}"#))
        );
        assert!(
            !args
                .iter()
                .any(|a| a.contains("danger") || a.contains("bypass") || a == "--sandbox")
        );
        let edit = command(&RunSpec {
            mode: TaskMode::Edit,
            ..spec
        })
        .unwrap();
        assert!(
            !edit
                .as_std()
                .get_args()
                .any(|a| a.to_string_lossy().contains("prodex_git_setup"))
        );
    }

    #[test]
    fn command_keeps_prompts_out_of_args_and_resume_explicit() {
        let spec = RunSpec {
            provider: Provider::Codex,
            cwd: "/tmp/project with spaces".into(),
            prompt: "do not put this in argv".into(),
            mode: TaskMode::ReadOnly,
            session_id: Some("session-123".into()),
        };
        let cmd = command(&spec).unwrap();
        let args: Vec<_> = cmd
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|a| a == ["--sandbox", "read-only"]));
        assert!(
            args.windows(2)
                .any(|a| a == ["--ask-for-approval", "never"])
        );
        assert!(args.ends_with(&["resume".into(), "session-123".into(), "-".into()]));
        assert!(!args.contains(&spec.prompt));
        assert!(!args.iter().any(|a| a.contains("bypass")));
        assert!(
            command(&RunSpec {
                session_id: Some("--last".into()),
                ..spec
            })
            .is_err()
        );
    }
}
