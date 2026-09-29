//! Conservative, read-only observation of interactive terminal Codex processes.
//! Process identities are deliberately not presented as provider thread IDs.
use crate::model::{Observation, ObservedSession, Project, now};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;

#[derive(Debug)]
struct Process {
    pid: u32,
    parent: u32,
    uid: u32,
    terminal: bool,
    started: String,
    args: Vec<String>,
}

fn processes(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 10 {
                return None;
            }
            Some(Process {
                pid: fields[0].parse().ok()?,
                parent: fields[1].parse().ok()?,
                uid: fields[2].parse().ok()?,
                terminal: !matches!(fields[3], "?" | "??" | "-"),
                started: fields[4..9].join("_"),
                args: fields[9..].iter().map(|s| s.to_string()).collect(),
            })
        })
        .collect()
}

// ps arguments are not shell-escaped. Accept only unambiguous recognized forms;
// unfamiliar options, positional prompts, and paths containing spaces fail closed.
fn interactive(process: &Process) -> Option<Option<PathBuf>> {
    if !process.terminal || Path::new(process.args.first()?).file_name()? != "codex" {
        return None;
    }
    let mut cwd = None;
    let mut args = process.args.iter().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-daemon" | "--no-alt-screen" | "--search" | "--approve-for-me" => {}
            "-C" | "--cd" => {
                cwd = Some(PathBuf::from(args.next()?));
            }
            "-m" | "--model" | "-p" | "--profile" | "-s" | "--sandbox" | "-a"
            | "--ask-for-approval" | "--enable" | "--disable" => {
                args.next()?;
            }
            "resume" => {
                let session = args.next()?;
                if !session.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                    || session.len() != 36
                {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if cwd.as_ref().is_some_and(|p| !p.is_absolute()) {
        return None;
    }
    Some(cwd)
}

fn owned(process: &Process, all: &HashMap<u32, &Process>) -> bool {
    let mut current = Some(process);
    let mut visited = HashSet::new();
    while let Some(p) = current {
        if !visited.insert(p.pid) {
            return true;
        }
        if p.pid == std::process::id()
            || p.args
                .first()
                .and_then(|s| Path::new(s).file_name())
                .is_some_and(|s| s == "prodex" || s == "prodex-desktop")
        {
            return true;
        }
        current = all.get(&p.parent).copied();
    }
    false
}

fn same_process(before: &Process, after: &Process) -> bool {
    before.pid == after.pid
        && before.started == after.started
        && before.uid == after.uid
        && before.parent == after.parent
        && before.args == after.args
        && after.terminal
}

fn managed_session(process: &Process, excluded_session_ids: &[String]) -> bool {
    process
        .args
        .iter()
        .any(|arg| excluded_session_ids.contains(arg))
}

fn directories(text: &str) -> HashMap<u32, PathBuf> {
    let mut result = HashMap::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        }
        if let (Some(pid), Some(value)) = (pid, line.strip_prefix('n')) {
            result.insert(pid, PathBuf::from(value));
        }
    }
    result
}

fn project_for<'a>(cwd: &Path, roots: &'a [PathBuf], excluded: &[PathBuf]) -> Option<&'a PathBuf> {
    if excluded.iter().any(|root| cwd.starts_with(root)) {
        return None;
    }
    roots
        .iter()
        .filter(|root| cwd.starts_with(root))
        .max_by_key(|root| root.components().count())
}

async fn output(program: &str, args: &[&str]) -> Result<String, String> {
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("{program} observation timed out"))?
    .map_err(|_| format!("{program} observation unavailable"))?;
    if !result.status.success() {
        return Err(format!("{program} observation denied or unavailable"));
    }
    if result.stdout.len() > 8 * 1024 * 1024 {
        return Err("Process observation exceeded size limit".into());
    }
    String::from_utf8(result.stdout).map_err(|_| "Process observation was not valid text".into())
}

/// Only enabled, explicitly permitted projects should be supplied by the caller.
/// Does not start Codex, inspect conversations, or attach to an existing turn.
pub async fn scan(
    projects: Vec<Project>,
    excluded_session_ids: Vec<String>,
    excluded_roots: Vec<PathBuf>,
) -> Observation {
    let checked_at = now();
    let result = scan_inner(projects, excluded_session_ids, excluded_roots, checked_at).await;
    match result {
        Ok(sessions) => Observation { checked_at: Some(checked_at), available: true,
            detail: "Observing local interactive Codex terminals. Desktop, remote, and unrecognized CLI forms are not observed.".into(), sessions },
        Err(detail) => Observation { checked_at: Some(checked_at), available: false, detail, sessions: vec![] },
    }
}

async fn scan_inner(
    projects: Vec<Project>,
    excluded_session_ids: Vec<String>,
    excluded_roots: Vec<PathBuf>,
    checked_at: u64,
) -> Result<Vec<ObservedSession>, String> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err("Terminal observation is supported on macOS and Linux only".into());
    }
    let roots: Vec<_> = projects
        .iter()
        .filter(|p| p.enabled)
        .filter_map(|p| p.path.canonicalize().ok())
        .collect();
    let excluded: Vec<_> = excluded_roots
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let list = processes(&output("ps", &["-wwaxo", "pid=,ppid=,uid=,tty=,lstart=,args="]).await?);
    if list.is_empty() {
        return Err("Process observation returned no readable processes".into());
    }
    let all: HashMap<_, _> = list.iter().map(|p| (p.pid, p)).collect();
    // Verify the executable name independently of the user-controlled argv text.
    let executable_output = output("ps", &["-axo", "pid=,comm="]).await?;
    let executable_pids: HashSet<u32> = executable_output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (pid, executable) = line.split_once(char::is_whitespace)?;
            (Path::new(executable.trim()).file_name()? == "codex")
                .then(|| pid.parse().ok())
                .flatten()
        })
        .collect();
    // SAFETY: geteuid has no arguments and no memory preconditions.
    let uid = unsafe { libc::geteuid() };
    let candidates: Vec<_> = list
        .iter()
        .filter(|p| p.uid == uid && executable_pids.contains(&p.pid) && !owned(p, &all))
        .filter(|p| !managed_session(p, &excluded_session_ids))
        .filter_map(|p| interactive(p).map(|cwd| (p, cwd)))
        .collect();
    if candidates.is_empty() || roots.is_empty() {
        return Ok(vec![]);
    }
    let pids = candidates
        .iter()
        .map(|(p, _)| p.pid.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let cwd_by_pid = directories(
        &output(
            "lsof",
            &["-n", "-P", "-a", "-p", &pids, "-d", "cwd", "-F", "pn"],
        )
        .await?,
    );
    // Recheck identity after filesystem inspection; PID reuse or a closed TUI
    // must not activate a project based on the first process snapshot.
    let current =
        processes(&output("ps", &["-wwaxo", "pid=,ppid=,uid=,tty=,lstart=,args="]).await?);
    let mut sessions = vec![];
    for (process, override_cwd) in candidates {
        if !current.iter().any(|p| same_process(process, p)) {
            continue;
        }
        let Some(process_cwd) = cwd_by_pid.get(&process.pid) else {
            continue;
        };
        let cwd = override_cwd.unwrap_or_else(|| process_cwd.clone());
        let Ok(cwd) = cwd.canonicalize() else {
            continue;
        };
        let Some(project) = project_for(&cwd, &roots, &excluded) else {
            continue;
        };
        let id = format!("codex-process:{}:{}", process.pid, process.started);
        if excluded_session_ids.contains(&id) {
            continue;
        }
        // A process that disappeared during the scan must not reactivate a project.
        if unsafe { libc::kill(process.pid as i32, 0) } != 0 {
            continue;
        }
        sessions.push(ObservedSession {
            id,
            cwd,
            project: project.clone(),
            last_seen: checked_at,
            source: "codex_terminal_process".into(),
        });
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn process(args: &str) -> Process {
        processes(&format!(
            "99999 1 501 ttys001 Tue Sep 29 09:36:45 2026 {args}"
        ))
        .remove(0)
    }
    #[tokio::test]
    #[ignore = "requires OS process visibility and an independent interactive Codex in the test project"]
    async fn live_terminal_observation() {
        let path = PathBuf::from(
            std::env::var("PRODEX_OBSERVATION_TEST_PROJECT").expect("set project path"),
        );
        let result = scan(
            vec![Project {
                use_worktrees: true,
                path,
                objective: "read-only observation test".into(),
                objective_version: 1,
                enabled: true,
            }],
            vec![],
            vec![],
        )
        .await;
        assert!(result.available, "{}", result.detail);
        assert!(
            !result.sessions.is_empty(),
            "expected an independent interactive Codex terminal"
        );
        assert!(
            result
                .sessions
                .iter()
                .all(|s| s.source == "codex_terminal_process")
        );
        eprintln!(
            "Observed {} live independent Codex terminals",
            result.sessions.len()
        );
    }

    #[test]
    fn accepts_live_interactive_forms_only() {
        assert_eq!(interactive(&process("codex")), Some(None));
        assert_eq!(
            interactive(&process("/bin/codex --no-daemon --cd /repo")),
            Some(Some("/repo".into()))
        );
        assert!(
            interactive(&process(
                "codex resume 01a0ec18-4092-7453-bbfc-653ddc7b1d2b"
            ))
            .is_some()
        );
        for args in [
            "codex exec --json",
            "codex app-server",
            "codex review",
            "codex --remote unix://",
            "codex --worktree",
            "codex arbitrary prompt",
            "codex resume --last",
            "codex --cd relative",
            "othercodex",
        ] {
            assert!(interactive(&process(args)).is_none(), "{args}");
        }
        let mut p = process("codex");
        p.terminal = false;
        assert!(interactive(&p).is_none());
    }
    #[test]
    fn parses_cwds_with_spaces_and_rejects_sibling_prefixes() {
        let paths = directories("p99999\nfcwd\nn/repo with space\np88888\nfcwd\nn/another\n");
        assert_eq!(paths[&99999], PathBuf::from("/repo with space"));
        let roots = vec!["/repo".into(), "/repo/nested".into()];
        assert_eq!(
            project_for(Path::new("/repo/nested/src"), &roots, &[]),
            Some(&roots[1])
        );
        assert!(project_for(Path::new("/repo-other"), &roots, &[]).is_none());
        assert!(
            project_for(
                Path::new("/repo/worktree/src"),
                &roots,
                &["/repo/worktree".into()]
            )
            .is_none()
        );
    }
    #[test]
    fn excludes_prodex_descendants_and_distinguishes_pid_reuse() {
        let mut parent = process("/bin/prodex daemon");
        parent.pid = 99998;
        let mut child = process("codex");
        child.parent = parent.pid;
        let all = HashMap::from([(parent.pid, &parent), (child.pid, &child)]);
        assert!(owned(&child, &all));
        let newer = processes("99999 1 501 ttys001 Tue Sep 29 10:36:45 2026 codex").remove(0);
        assert!(!same_process(&child, &newer));
        assert!(same_process(&child, &child));
        assert!(processes("").is_empty()); // no archived histories are a source
    }

    #[test]
    fn resumed_managed_sessions_cannot_activate_work() {
        let id = "01a0ec18-4092-7453-bbfc-653ddc7b1d2b".to_string();
        let resumed = process(&format!("codex resume {id}"));
        assert!(managed_session(&resumed, &[id]));
        assert!(!managed_session(&process("codex"), &["other".into()]));
        assert!(interactive(&process("claude")).is_none());
    }

    #[test]
    fn canonical_matching_resolves_symlinks_without_authorizing_external_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&root, dir.path().join("alias")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let roots = vec![root.canonicalize().unwrap()];
        let cwd = dir.path().join("alias/nested").canonicalize().unwrap();
        assert_eq!(project_for(&cwd, &roots, &[]), Some(&roots[0]));
        let cwd = root.join("escape").canonicalize().unwrap();
        assert!(project_for(&cwd, &roots, &[]).is_none());
    }
}
