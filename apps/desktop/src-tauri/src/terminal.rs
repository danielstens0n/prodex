use prodex_core::model::{Provider, TaskRecord};

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn resume_command(task: &TaskRecord) -> Result<String, String> {
    let session = task
        .session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or("This suggestion has no session yet. Approve it first.")?;
    let provider = match task.proposal.provider {
        Provider::Codex => "codex resume --include-non-interactive",
        Provider::Claude => "claude --resume",
        Provider::Mock => return Err("Mock tasks have no terminal session.".into()),
    };
    let cwd = task.worktree.as_ref().unwrap_or(&task.proposal.project);
    let cwd = cwd.to_str().ok_or("Project path is not valid text")?;
    if cwd.contains('\0') || session.contains('\0') || session.starts_with('-') {
        return Err("Invalid session path or ID".into());
    }
    Ok(format!(
        "cd -- {} && {provider} {}",
        quote(cwd),
        quote(session)
    ))
}

/// Explicit desktop action: continue the existing session with a focused handoff.
/// Completion never launches this automatically or implies that integration happened.
pub fn completion_command(task: &TaskRecord, action: &str) -> Result<String, String> {
    if action != "terminal" {
        return Err("Use Merge locally in Prodex to run a verified background integration.".into());
    }
    resume_command(task)
}

#[cfg(target_os = "macos")]
pub fn open(command: &str) -> Result<(), String> {
    // Pass the command as data, never interpolate it into AppleScript source.
    let script = r#"on run argv
with timeout of 30 seconds
tell application "Terminal"
activate
do script (item 1 of argv)
end tell
end timeout
end run"#;
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", script, "--", command])
        .output()
        .map_err(|_| "Could not open Terminal. Use Copy command instead.".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("Could not open Terminal. Allow Prodex to control Terminal in macOS System Settings → Privacy & Security → Automation, or use Copy command.".into())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open(_command: &str) -> Result<(), String> {
    Err("Open in Terminal is currently available on macOS. Use Copy command instead.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prodex_core::model::*;
    fn task() -> TaskRecord {
        TaskRecord {
            attempts: Vec::new(),
            id: "task".into(),
            automatic: true,
            started_at: None,
            proposal: TaskProposal {
                brief: None,
                project: "/projects/it's $(echo nope)".into(),
                objective_version: 1,
                prompt: "Task".into(),
                rationale: "Reason".into(),
                provider: Provider::Codex,
                mode: TaskMode::ReadOnly,
                dependencies: vec![],
                expected_files: vec![],
                completion_criteria: "Done".into(),
                risk: RiskLevel::Low,
            },
            status: TaskStatus::Succeeded,
            review: ReviewStatus::NotRequired,
            session_id: Some("session-id".into()),
            worktree: None,
            branch: None,
            base_commit: None,
            pid: None,
            summary: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }
    #[test]
    fn commands_quote_paths_and_use_the_correct_provider_and_worktree() {
        let mut task = task();
        assert_eq!(resume_command(&task).unwrap(), "cd -- '/projects/it'\\''s $(echo nope)' && codex resume --include-non-interactive 'session-id'");
        task.proposal.provider = Provider::Claude;
        task.worktree = Some("/worktrees/task".into());
        assert_eq!(
            resume_command(&task).unwrap(),
            "cd -- '/worktrees/task' && claude --resume 'session-id'"
        );
    }
    #[test]
    fn local_merge_and_publishing_cannot_be_started_through_terminal_handoff() {
        let task = task();
        assert_eq!(
            completion_command(&task, "terminal").unwrap(),
            resume_command(&task).unwrap()
        );
        assert!(completion_command(&task, "merge").is_err());
        assert!(completion_command(&task, "pr").is_err());
    }

    #[test]
    fn live_sessions_can_be_opened_but_uncreated_and_mock_sessions_cannot() {
        let mut task = task();
        for status in [
            TaskStatus::Starting,
            TaskStatus::Running,
            TaskStatus::Stopping,
            TaskStatus::RecoveryRequired,
        ] {
            task.status = status;
            assert!(resume_command(&task).unwrap().contains("codex resume"));
        }
        task.status = TaskStatus::AwaitingApproval;
        task.session_id = None;
        assert!(resume_command(&task).is_err());
        task.session_id = Some("session".into());
        task.proposal.provider = Provider::Mock;
        assert!(resume_command(&task).is_err());
    }
}
