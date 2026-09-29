# Isolated task workspaces

Editing tasks get a Git worktree on a new `prodex/<task-id>` branch. The source
checkout's `HEAD` commit is recorded before creation and passed explicitly to Git,
so a concurrent source branch movement does not change the chosen base.

The manager accepts only ASCII letters, digits, and hyphens in task IDs. It
resolves the repository and existing directory prefixes, rejects a worktree root
inside the source checkout (including through symlinks), and refuses to overwrite
an existing destination or branch. A project without a commit cannot create a
task worktree. The worktree manager does not initialize repositories. The coordinator separately proposes an approval-required `initialize_repository` task for a missing repository or initial commit. With worktrees enabled, only setup runs directly in the project folder, exclusively; ordinary edits wait for a verified HEAD. Users can explicitly opt into main-folder coding as described below. Parent repositories are detected to avoid nested initialization.

Uncommitted changes in the source checkout are preserved and **not copied** to
the task. Scheduling must postpone tasks that depend on those changes. Worktrees
do not isolate credentials, ports, databases, or other machine resources.

The current diff helper returns tracked changes against `HEAD`, including staged
and unstaged changes. It disables external diff helpers and text converters.
Untracked files are excluded; a complete review interface must separately list
them. This helper does not show already committed task changes relative to the
recorded base; that comparison belongs in the result-review integration.

Git hooks are disabled for manager operations using a per-command configuration
override. Checkout can still invoke repository-configured smudge/process filters;
only enable editing in trusted repositories. Each Git operation has a 60-second
deadline, concurrent bounded stdout/stderr capture, and its own process group.
On timeout the group is killed and partial artifacts are retained. Output above
8 MiB per stream produces an error rather than consuming unbounded memory.

There is no cleanup API. Worktrees, branches, and any partial artifacts left by a
Git failure are retained for inspection. Do not retry a partially created task
under the same ID without reconciling Git's state. The initial manager assumes a
trusted local filesystem; it does not defend against hostile processes swapping
symlinks between path validation and Git execution.

Tests use temporary repositories only and cover source-change preservation,
isolated writes, branch/destination collisions, invalid task IDs, internal and
symlinked roots, diff behavior, and non-Git projects.


## Optional main-folder coding

Projects default to `use_worktrees: true`. Settings → Workspaces can disable it
for one project. The coordinator then launches `edit_in_place` in the original
folder, without requiring Git or creating a worktree. It serializes Prodex tasks
in that project, preserves normal provider permissions and asks the worker to
preserve existing edits. This cannot serialize human or independently launched
agent edits. Workspace settings cannot change during an active managed task.
Existing worktree artifacts and results are preserved across mode changes.

Completed coding results remain awaiting review until **Merge locally** runs its managed integration job and Git verification passes. Verification pins the destination branch/history, requires a clean committed result descending from the recorded task base and present in destination history, and compares unrelated tracked/staged/untracked changes with the prelaunch snapshot. A successful model response alone does not integrate a result. Stop, retry and crash recovery preserve worktrees. Unrelated concurrent edits can prevent verification; no automatic stash/reset/cleanup is attempted. Main-folder results still use explicit **Mark reviewed**.
