//! Isolated editing workspaces. Creation never copies uncommitted source changes,
//! resets a branch, or removes an existing checkout. Cleanup is deliberately absent.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Workspace {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryState {
    Ready,
    MissingRepository,
    MissingCommit,
}

/// Missing Git metadata is a setup condition; broken metadata and Git errors are not.
pub fn repository_state(project: &Path) -> Result<RepositoryState> {
    let project = project
        .canonicalize()
        .context("resolve project directory")?;
    let output = bounded_output(
        git_command(&project).args(["rev-parse", "--show-toplevel"]),
        GIT_TIMEOUT,
    )?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let metadata_exists = project
            .ancestors()
            .any(|p| std::fs::symlink_metadata(p.join(".git")).is_ok());
        if !metadata_exists && error.contains("not a git repository") {
            return Ok(RepositoryState::MissingRepository);
        }
        bail!("Cannot inspect Git repository: {}", error.trim());
    }
    let root = PathBuf::from(String::from_utf8(output.stdout)?.trim()).canonicalize()?;
    let head = bounded_output(
        git_command(&project).args(["rev-parse", "--verify", "HEAD^{commit}"]),
        GIT_TIMEOUT,
    )?;
    if head.status.success() {
        return Ok(RepositoryState::Ready);
    }
    let reference = git(&project, &["symbolic-ref", "-q", "HEAD"])?;
    let exists = bounded_output(
        git_command(&project).args(["show-ref", "--verify", "--quiet", reference.trim()]),
        GIT_TIMEOUT,
    )?;
    if exists.status.code() == Some(1) {
        if root != project {
            bail!(
                "The parent Git repository needs its first commit; add the repository root as the project to set it up"
            );
        }
        return Ok(RepositoryState::MissingCommit);
    }
    bail!("Git HEAD is invalid; repair the repository before coding")
}

/// Create a task branch at the source repository's current commit.
///
/// The worktree root must be outside the source checkout (including through
/// symlinks). Existing branches and destinations are errors, never overwritten.
/// If Git partially succeeds then fails, artifacts remain for manual inspection.
/// Git hooks are disabled. Checkout may still execute repository-configured
/// smudge/process filters, so repositories must be trusted; subprocess groups
/// are terminated if an individual Git operation exceeds sixty seconds.
pub fn create_worktree(project: &Path, worktree_root: &Path, task_id: &str) -> Result<Workspace> {
    if task_id.is_empty()
        || !task_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        bail!("task ID must contain only ASCII letters, digits, and hyphens");
    }
    let project = repository_root(project)?;
    let base_commit = git(&project, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let base_commit = base_commit.trim().to_owned();
    let root = resolve_future_directory(worktree_root)?;
    if root.starts_with(&project) {
        bail!("worktree root must be outside the source checkout");
    }
    let path = root.join(task_id);
    // symlink_metadata catches dangling symlinks as well as existing directories.
    match std::fs::symlink_metadata(&path) {
        Ok(_) => bail!("worktree destination already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspect worktree destination"),
    }
    std::fs::create_dir_all(&root).context("create worktree root")?;
    let branch = format!("prodex/{task_id}");
    let mut command = git_command(&project);
    command
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&path)
        .arg(&base_commit);
    let output = bounded_output(&mut command, GIT_TIMEOUT).context("git worktree add")?;
    if !output.status.success() {
        bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(Workspace {
        path,
        branch,
        base_commit,
    })
}

/// Return staged and unstaged tracked changes relative to HEAD.
/// Untracked files are not included. External diff programs and text converters
/// are disabled so inspecting a result does not execute repository diff helpers.
pub fn diff(project: &Path) -> Result<String> {
    let project = repository_root(project)?;
    git(
        &project,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "HEAD",
            "--",
        ],
    )
}

fn repository_root(project: &Path) -> Result<PathBuf> {
    let project = project
        .canonicalize()
        .context("resolve project directory")?;
    let root = git(&project, &["rev-parse", "--show-toplevel"])?;
    Path::new(root.trim_end_matches(['\r', '\n']))
        .canonicalize()
        .context("resolve repository root")
}

fn git(project: &Path, args: &[&str]) -> Result<String> {
    let mut command = git_command(project);
    command.args(args);
    let output = bounded_output(&mut command, GIT_TIMEOUT).context("run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("Git output is not valid UTF-8")
}

fn git_command(project: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(project)
        .args(["-c", "core.hooksPath=/dev/null"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    command
}

// Readers drain both pipes concurrently. Completion also waits for pipe EOF,
// under the same deadline: a child exiting while its descendant retains a pipe
// must not leave us blocked in a reader thread join.
fn bounded_output(command: &mut Command, timeout: Duration) -> Result<Output> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start Git subprocess")?;
    let group_id = child.id();
    let (sender, receiver) = mpsc::channel();
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let stdout_sender = sender.clone();
    std::thread::spawn(move || {
        let _ = stdout_sender.send((true, read_bounded(stdout)));
    });
    std::thread::spawn(move || {
        let _ = sender.send((false, read_bounded(stderr)));
    });
    let deadline = Instant::now() + timeout;
    let mut status = None;
    let mut stdout = None;
    let mut stderr = None;
    loop {
        while let Ok((is_stdout, result)) = receiver.try_recv() {
            if is_stdout {
                stdout = Some(result);
            } else {
                stderr = Some(result);
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(value) => status = value,
                Err(error) => {
                    terminate_group(&mut child, group_id);
                    return Err(error).context("poll Git subprocess");
                }
            }
        }
        if let Some(status) = status
            && stdout.is_some()
            && stderr.is_some()
        {
            return Ok(Output {
                status,
                stdout: stdout.take().unwrap().context("read Git stdout")?,
                stderr: stderr.take().unwrap().context("read Git stderr")?,
            });
        }
        if Instant::now() >= deadline {
            terminate_group(&mut child, group_id);
            bail!(
                "Git operation exceeded {} second timeout; partial workspace artifacts preserved",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_bounded(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut overflow = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let available = MAX_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(available)]);
        overflow |= count > available;
    }
    if overflow {
        return Err(std::io::Error::other("Git output exceeds 8 MiB limit"));
    }
    Ok(output)
}

fn terminate_group(child: &mut std::process::Child, group_id: u32) {
    #[cfg(unix)]
    // SAFETY: this is the subprocess group created above, never the caller's.
    unsafe {
        libc::kill(-(group_id as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

// Resolve the existing prefix before creating missing directories, so a rejected
// root does not leave directories in the user's checkout.
fn resolve_future_directory(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    match absolute.canonicalize() {
        Ok(resolved) => {
            if !resolved.is_dir() {
                bail!("worktree root is not a directory: {}", resolved.display());
            }
            Ok(resolved)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Do not traverse an existing dangling symlink.
            if absolute.symlink_metadata().is_ok() {
                bail!("cannot resolve worktree root: {}", absolute.display());
            }
            let name = absolute
                .file_name()
                .context("worktree root has an unresolved parent component")?;
            let parent = absolute.parent().context("worktree root has no parent")?;
            Ok(resolve_future_directory(parent)?.join(name))
        }
        Err(error) => Err(error).context("resolve worktree root"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init"]).unwrap();
        git(&repo, &["config", "user.name", "Prodex Test"]).unwrap();
        git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), "committed\n").unwrap();
        git(&repo, &["add", "tracked.txt"]).unwrap();
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        )
        .unwrap();
        (dir, repo)
    }

    #[test]
    fn worktree_is_based_on_commit_and_preserves_main_changes() {
        let (dir, repo) = repository();
        std::fs::write(repo.join("tracked.txt"), "user changes\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "private draft\n").unwrap();
        let status_before = git(&repo, &["status", "--porcelain"]).unwrap();
        let workspace = create_worktree(&repo, &dir.path().join("worktrees"), "task-1").unwrap();
        assert_eq!(workspace.branch, "prodex/task-1");
        assert_eq!(
            workspace.base_commit,
            git(&repo, &["rev-parse", "HEAD"]).unwrap().trim()
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("tracked.txt")).unwrap(),
            "committed\n"
        );
        assert!(!workspace.path.join("untracked.txt").exists());
        assert_eq!(
            git(&repo, &["status", "--porcelain"]).unwrap(),
            status_before
        );
        std::fs::write(workspace.path.join("tracked.txt"), "worker changes\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("tracked.txt")).unwrap(),
            "user changes\n"
        );
        assert!(diff(&workspace.path).unwrap().contains("+worker changes"));
    }

    #[test]
    fn rejects_unsafe_ids_and_internal_roots_without_creating_them() {
        let (dir, repo) = repository();
        for id in ["", "../escape", "a/b", "a b", "ä", "task;echo"] {
            assert!(create_worktree(&repo, &dir.path().join("worktrees"), id).is_err());
        }
        assert!(!dir.path().join("worktrees").exists());
        assert!(create_worktree(&repo, &repo.join("nested/worktrees"), "task").is_err());
        assert!(!repo.join("nested").exists());
    }

    #[test]
    fn existing_destination_and_branch_are_preserved() {
        let (dir, repo) = repository();
        let root = dir.path().join("worktrees");
        let workspace = create_worktree(&repo, &root, "task").unwrap();
        std::fs::write(workspace.path.join("draft"), "preserve").unwrap();
        assert!(create_worktree(&repo, &root, "task").is_err());
        assert_eq!(
            std::fs::read_to_string(workspace.path.join("draft")).unwrap(),
            "preserve"
        );
        assert!(create_worktree(&repo, &dir.path().join("other"), "task").is_err());
        assert!(!dir.path().join("other/task").exists());
    }

    #[test]
    fn diff_includes_staged_and_unstaged_changes() {
        let (_dir, repo) = repository();
        std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
        git(&repo, &["add", "staged.txt"]).unwrap();
        std::fs::write(repo.join("tracked.txt"), "unstaged\n").unwrap();
        let result = diff(&repo).unwrap();
        assert!(result.contains("+staged"));
        assert!(result.contains("+unstaged"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_into_source_checkout() {
        let (dir, repo) = repository();
        let link = dir.path().join("alias");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        assert!(create_worktree(&repo, &link.join("new/root"), "task").is_err());
        assert!(!repo.join("new").exists());
    }

    #[test]
    fn repository_detection_distinguishes_setup_from_broken_metadata() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            repository_state(dir.path()).unwrap(),
            RepositoryState::MissingRepository
        );
        assert!(!dir.path().join(".git").exists());
        git(dir.path(), &["init"]).unwrap();
        assert_eq!(
            repository_state(dir.path()).unwrap(),
            RepositoryState::MissingCommit
        );
        let nested = dir.path().join("src");
        std::fs::create_dir(&nested).unwrap();
        assert!(repository_state(&nested).is_err());
        git(
            dir.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        )
        .unwrap();
        assert_eq!(
            repository_state(dir.path()).unwrap(),
            RepositoryState::Ready
        );
        assert_eq!(repository_state(&nested).unwrap(), RepositoryState::Ready);
        std::fs::write(dir.path().join(".git/HEAD"), "broken metadata").unwrap();
        assert!(repository_state(dir.path()).is_err());
    }

    #[test]
    fn rejects_non_git_projects_without_initializing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create_worktree(dir.path(), &dir.path().join("worktrees"), "task").is_err());
        assert!(!dir.path().join(".git").exists());
    }

    #[test]
    fn rejects_unborn_repository_without_creating_root() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init"]).unwrap();
        let root = dir.path().join("worktrees");
        assert!(create_worktree(&repo, &root, "task").is_err());
        assert!(!root.exists());
    }

    #[test]
    fn resolves_project_subdirectory_to_repository_root() {
        let (dir, repo) = repository();
        let subdir = repo.join("src");
        std::fs::create_dir(&subdir).unwrap();
        // A sibling of src is still inside the checkout and must be rejected.
        assert!(create_worktree(&subdir, &repo.join("worktrees"), "task").is_err());
        let workspace = create_worktree(&subdir, &dir.path().join("worktrees"), "task").unwrap();
        assert!(workspace.path.join("tracked.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn worktree_creation_does_not_execute_post_checkout_hook() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = repository();
        let hooks = repo.join(".git/hooks");
        git(
            &repo,
            &["config", "core.hooksPath", hooks.to_str().unwrap()],
        )
        .unwrap();
        let hook = hooks.join("post-checkout");
        std::fs::write(&hook, "#!/bin/sh\nprintf ran > hook-ran\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let workspace = create_worktree(&repo, &dir.path().join("worktrees"), "task").unwrap();
        assert!(workspace.path.join("tracked.txt").exists());
        assert!(!workspace.path.join("hook-ran").exists());
        assert!(!repo.join("hook-ran").exists());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_covers_descendants_holding_pipes_after_parent_exits() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & exit 0"]);
        let start = Instant::now();
        let result = bounded_output(&mut command, Duration::from_millis(100));
        assert!(result.unwrap_err().to_string().contains("timeout"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn drains_stdout_and_stderr_concurrently() {
        let mut command = Command::new("sh");
        command.args(["-c", "i=0; while [ $i -lt 10000 ]; do printf 'abcdefghij'; printf 'klmnopqrst' >&2; i=$((i+1)); done"]);
        let output = bounded_output(&mut command, Duration::from_secs(10)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 100000);
        assert_eq!(output.stderr.len(), 100000);
    }
}
