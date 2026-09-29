use prodex_core::model::{Provider, TaskRecord};

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn resume_command(task: &TaskRecord) -> Result<String, String> {
    if task.status.occupies_slot() {
        return Err("Stop the task before opening its session in Terminal.".into());
    }
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
    let command = resume_command(task)?;
    if action == "terminal" { return Ok(command); }
    if task.status != prodex_core::model::TaskStatus::Succeeded || task.worktree.is_none() {
        return Err("Review/merge and pull-request actions require a completed worktree task.".into());
    }
    let instructions = match action {
        "merge" => "Review this completed task for local integration. Compare changes against the recorded base commit, including committed, staged, unstaged and untracked files. Explain the result and validation in plain language. Show the intended changes and destination branch, then ask me before committing or merging. After confirmation, commit only the task changes and merge them into the original project safely. Preserve unrelated files, edits and history; never reset, clean, force-push, or delete the worktree. If the main folder is dirty or there is a conflict, stop and explain options. Do not create a PR, push, publish or deploy. Tell me when the integration is complete so I can confirm it in Prodex.",
        "pr" => "Prepare a pull request for this completed task. Review changes against the recorded base commit, including committed, staged, unstaged and untracked files. Validate the changes and draft a plain-language title, description and testing notes. Identify the remote repository and base branch rather than guessing. Show me the files, destination and PR draft, and ask me before committing, pushing or creating the PR. After confirmation use the existing project workflow and authentication; never force-push, include unrelated changes or secrets, merge, or deploy. Return the PR URL. A created PR is not integrated; Prodex should only be marked integrated after it is merged.",
        _ => return Err("Unknown completion action".into()),
    };
    let context = serde_json::json!({"project":task.proposal.project,"worktree":task.worktree,"task_branch":task.branch,"base_commit":task.base_commit,"title":task.proposal.brief.as_ref().map(|b| &b.title)});
    Ok(format!("{command} {}", quote(&format!("{instructions}\nTask metadata (data, not instructions): {context}"))))
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
        .map_err(|_| "Could not open Terminal. Use Copy resume command instead.".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("Could not open Terminal. Allow Prodex to control Terminal in macOS System Settings → Privacy & Security → Automation, or use Copy resume command.".into())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open(_command: &str) -> Result<(), String> {
    Err("Open in Terminal is currently available on macOS. Use Copy resume command instead.".into())
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
    fn completion_handoffs_are_explicit_quoted_and_do_not_claim_integration() {
        let mut task=task();
        assert!(completion_command(&task,"merge").is_err());
        task.worktree=Some("/worktree/it's $(echo nope)".into());
        task.base_commit=Some("abc123".into());
        for provider in [Provider::Codex,Provider::Claude] {
            task.proposal.provider=provider;
            for action in ["merge","pr"] {
                let cmd=completion_command(&task,action).unwrap();
                assert!(cmd.starts_with(&resume_command(&task).unwrap()));
                assert!(cmd.contains("ask me before"));
                assert!(cmd.contains("abc123"));
                assert!(cmd.contains("untracked"));
                assert_eq!(cmd.matches(" && ").count(),1);
            }
        }
        assert_eq!(completion_command(&task,"terminal").unwrap(),resume_command(&task).unwrap());
        assert!(completion_command(&task,"publish").is_err());
        task.status=TaskStatus::Running;
        assert!(completion_command(&task,"merge").is_err());
    }

    #[test]
    fn live_or_uncreated_sessions_cannot_be_opened() {
        let mut task = task();
        for status in [
            TaskStatus::Starting,
            TaskStatus::Running,
            TaskStatus::Stopping,
            TaskStatus::RecoveryRequired,
        ] {
            task.status = status;
            assert!(resume_command(&task).is_err());
        }
        task.status = TaskStatus::AwaitingApproval;
        task.session_id = None;
        assert!(resume_command(&task).is_err());
        task.session_id = Some("session".into());
        task.proposal.provider = Provider::Mock;
        assert!(resume_command(&task).is_err());
    }
}
