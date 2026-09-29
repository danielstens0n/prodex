//! Local app discovery and explicit session handoff. Never synthesize keystrokes
//! or change project/editor configuration to inject a terminal command.
use prodex_core::model::{Provider, TaskRecord};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct Destination {
    pub id: String,
    pub label: String,
    pub kind: String,
}
struct App {
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    names: &'static [&'static str],
}
const APPS: &[App] = &[
    App {
        id: "terminal",
        label: "Terminal",
        kind: "terminal",
        names: &["Terminal.app"],
    },
    App {
        id: "ghostty",
        label: "Ghostty",
        kind: "terminal",
        names: &["Ghostty.app"],
    },
    App {
        id: "iterm",
        label: "iTerm",
        kind: "terminal",
        names: &["iTerm.app", "iTerm2.app"],
    },
    App {
        id: "codex",
        label: "Codex",
        kind: "codex",
        names: &["Codex.app"],
    },
    App {
        id: "claude",
        label: "Claude Code",
        kind: "claude",
        names: &["Claude.app"],
    },
    App {
        id: "zed",
        label: "Zed",
        kind: "editor",
        names: &["Zed.app"],
    },
    App {
        id: "vscode",
        label: "VS Code",
        kind: "editor",
        names: &["Visual Studio Code.app"],
    },
    App {
        id: "cursor",
        label: "Cursor",
        kind: "editor",
        names: &["Cursor.app"],
    },
    App {
        id: "cmux",
        label: "cmux",
        kind: "clipboard",
        names: &["cmux.app"],
    },
    App {
        id: "warp",
        label: "Warp",
        kind: "clipboard",
        names: &["Warp.app"],
    },
    App {
        id: "wezterm",
        label: "WezTerm",
        kind: "clipboard",
        names: &["WezTerm.app"],
    },
    App {
        id: "kitty",
        label: "kitty",
        kind: "clipboard",
        names: &["kitty.app"],
    },
    App {
        id: "alacritty",
        label: "Alacritty",
        kind: "clipboard",
        names: &["Alacritty.app"],
    },
];
fn roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots.extend(
        [
            "/Applications",
            "/System/Applications/Utilities",
            "/Applications/Utilities",
        ]
        .map(PathBuf::from),
    );
    roots
}
fn locate(app: &App, roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .flat_map(|root| app.names.iter().map(move |name| root.join(name)))
        .find(|path| path.join("Contents/Info.plist").is_file())
}
pub fn installed() -> Vec<Destination> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }
    let roots = roots();
    APPS.iter()
        .filter(|app| locate(app, &roots).is_some())
        .map(|app| Destination {
            id: app.id.into(),
            label: app.label.into(),
            kind: app.kind.into(),
        })
        .collect()
}
pub fn custom(path: &Path) -> Result<Destination, String> {
    let path = path
        .canonicalize()
        .map_err(|_| "Application is no longer installed")?;
    if path.extension().is_none_or(|ext| ext != "app")
        || !path.join("Contents/Info.plist").is_file()
    {
        return Err("Choose a macOS application (.app).".into());
    }
    let label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("Invalid application name")?;
    Ok(Destination {
        id: format!("app:{}", path.display()),
        label: label.into(),
        kind: "clipboard".into(),
    })
}
fn resolve(id: &str) -> Result<(PathBuf, String, String), String> {
    if let Some(path) = id.strip_prefix("app:") {
        let app = custom(Path::new(path))?;
        return Ok((PathBuf::from(path), app.label, app.kind));
    }
    let app = APPS
        .iter()
        .find(|app| app.id == id)
        .ok_or("Unknown application")?;
    let path = locate(app, &roots()).ok_or_else(|| {
        format!(
            "{} is no longer installed. Choose another app or Copy command.",
            app.label
        )
    })?;
    Ok((path, app.label.into(), app.kind.into()))
}
#[derive(Debug, PartialEq)]
struct Invocation {
    program: String,
    args: Vec<String>,
}
fn invocation(
    id: &str,
    app: &Path,
    command: &str,
    cwd: &Path,
    task: &TaskRecord,
) -> Result<Invocation, String> {
    let app = app.to_str().ok_or("Invalid application path")?;
    let cwd = cwd.to_str().ok_or("Invalid project path")?;
    let mut args = vec!["-a".into(), app.into()];
    match id {
        "ghostty" => {
            args = vec![
                "-n".into(),
                "-a".into(),
                app.into(),
                "--args".into(),
                "-e".into(),
                "/bin/zsh".into(),
                "-lic".into(),
                command.into(),
            ];
        }
        "codex" => {
            if task.proposal.provider != Provider::Codex {
                return Err("This is not a Codex session. Choose a terminal or editor.".into());
            }
            let session = task.session_id.as_deref().ok_or("No session yet")?;
            if !session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || session.is_empty()
            {
                return Err("Invalid Codex session ID".into());
            }
            args.push(format!("codex://threads/{session}"));
        }
        "claude" => {
            if task.proposal.provider != Provider::Claude {
                return Err("This is not a Claude session. Choose a terminal or editor.".into());
            }
        }
        "zed" | "cursor" | "vscode" => {
            args.push(cwd.into());
        }
        _ => {}
    }
    Ok(Invocation {
        program: "/usr/bin/open".into(),
        args,
    })
}
/// Returns a notice and optional clipboard text, without claiming the receiving
/// app actually resumed a thread merely because LaunchServices accepted the open.
pub fn launch(
    task: &TaskRecord,
    action: &str,
    destination: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let command = crate::terminal::completion_command(task, action)?;
    let (app, label, kind) = resolve(destination)?;
    if destination == "terminal" {
        crate::terminal::open(&command)?;
        return Ok((None, None));
    }
    if destination == "iterm" {
        let script = r#"on run argv
with timeout of 30 seconds
tell application "iTerm"
activate
set newWindow to (create window with default profile)
tell current session of newWindow to write text (item 1 of argv)
end tell
end timeout
end run"#;
        execute(Invocation {
            program: "/usr/bin/osascript".into(),
            args: vec!["-e".into(), script.into(), "--".into(), command],
        })?;
        return Ok((None, None));
    }
    let cwd = task.worktree.as_ref().unwrap_or(&task.proposal.project);
    execute(invocation(destination, &app, &command, cwd, task)?)?;
    match kind.as_str() {
        "terminal" => Ok((None,None)),
        "codex" => Ok((None,None)),
        "claude" => Ok((Some("Opened Claude Code. Use /resume to select your CLI session. Terminal resume command copied.".into()),Some(command))),
        "editor" => Ok((Some(format!("Opened {label}. Resume command copied—paste it in the integrated terminal.")),Some(command))),
        _ => Ok((Some(format!("Opened {label}. Resume command copied—paste it in your terminal.")),Some(command))),
    }
}
fn execute(invocation: Invocation) -> Result<(), String> {
    let output = std::process::Command::new(&invocation.program)
        .args(&invocation.args)
        .output()
        .map_err(|e| format!("Could not open application: {e}. Use Copy command instead."))?;
    if output.status.success() {
        Ok(())
    } else {
        Err("Could not open the application. Check macOS Automation permissions if needed, or use Copy command.".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn task() -> TaskRecord {
        serde_json::from_value(serde_json::json!({
            "id":"task", "automatic":false, "attempts":[], "status":"running", "review":"not_required",
            "session_id":"session-123", "worktree":null,"branch":null,"base_commit":null,"pid":null,"started_at":null,
            "summary":"", "created_at":0,"updated_at":0,
            "proposal":{"project":"/tmp/project with 'quotes'", "objective_version":1,"prompt":"task","rationale":"reason",
                "provider":"codex","mode":"edit","risk":"low","dependencies":[],"expected_files":[],"completion_criteria":"done"}
        })).unwrap()
    }
    #[test]
    fn launch_arguments_preserve_commands_and_paths_as_data() {
        let task = task();
        let command = "cd -- '/tmp/project' && codex resume 'session-123'";
        let call = invocation(
            "ghostty",
            Path::new("/Applications/Ghostty.app"),
            command,
            &task.proposal.project,
            &task,
        )
        .unwrap();
        assert_eq!(call.program, "/usr/bin/open");
        assert_eq!(
            call.args,
            vec![
                "-n",
                "-a",
                "/Applications/Ghostty.app",
                "--args",
                "-e",
                "/bin/zsh",
                "-lic",
                command
            ]
        );
        for editor in ["zed", "cursor", "vscode"] {
            let call = invocation(
                editor,
                Path::new("/Applications/Editor.app"),
                command,
                &task.proposal.project,
                &task,
            )
            .unwrap();
            assert_eq!(call.args.last().unwrap(), "/tmp/project with 'quotes'");
            assert!(!call.args.iter().any(|a| a == command));
        }
    }
    #[test]
    fn direct_thread_links_reject_wrong_provider_and_url_injection() {
        let mut task = task();
        let call = invocation(
            "codex",
            Path::new("/Applications/Codex.app"),
            "",
            &task.proposal.project,
            &task,
        )
        .unwrap();
        assert_eq!(call.args.last().unwrap(), "codex://threads/session-123");
        assert!(invocation(
            "claude",
            Path::new("/Applications/Claude.app"),
            "",
            &task.proposal.project,
            &task
        )
        .is_err());
        task.session_id = Some("session?prompt=oops".into());
        assert!(invocation(
            "codex",
            Path::new("/Applications/Codex.app"),
            "",
            &task.proposal.project,
            &task
        )
        .is_err());
        task.proposal.provider = Provider::Claude;
        assert!(invocation(
            "codex",
            Path::new("/Applications/Codex.app"),
            "",
            &task.proposal.project,
            &task
        )
        .is_err());
    }
    #[test]
    fn discovery_requires_an_app_bundle() {
        let roots = vec![PathBuf::from("/definitely-missing")];
        assert!(locate(&APPS[0], &roots).is_none());
        assert!(custom(Path::new("/tmp")).is_err());
        assert!(resolve("unknown").is_err());
    }
    #[test]
    fn installed_apps_have_unique_supported_ids() {
        let apps = installed();
        let ids: std::collections::HashSet<_> = apps.iter().map(|a| &a.id).collect();
        assert_eq!(ids.len(), apps.len());
        for app in apps {
            assert!(resolve(&app.id).is_ok());
        }
    }
}
