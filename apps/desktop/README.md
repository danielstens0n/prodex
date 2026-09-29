# Prodex desktop

A resizable 680 × 760 Tauri window (minimum 540 × 620) with Activity and Settings tabs.
White surfaces, charcoal text, clear blue accents, and locally bundled Public Sans typography.
Semantic green, amber, and red distinguish success, waiting/retry, and error states.
The wordmark uses upright Space Grotesk Regular at 24px, written “Prodex”. Controls use the locally bundled
[Web Awesome](https://webawesome.com/) library. See the [visual guidelines](../../docs/visual-guidelines.md)
for reference-site findings, design tokens, and font licensing.

From this directory, using Node 22.12 or newer:

```sh
npm ci
npm run tauri dev
```

With the installed Volta toolchain:

```sh
volta run --node 24.20.0 --npm 11.19.0 npm run tauri dev
```

Start the daemon separately using [the development guide](../../docs/development.md).
Both processes use `PRODEX_STATE_DIR`, or `$HOME/.local/state/prodex` by default.
After rebuilding the daemon or changing the native window size, restart the daemon
and desktop application; frontend hot reload alone cannot apply those changes.

Keep `app.windows[].dragDropEnabled` set to `false` in `src-tauri/tauri.conf.json`.
Activity and Settings use HTML drag-and-drop for reordering; Tauri's native drop
handler can intercept these events before they reach the page. Changes to this
setting require rebuilding and restarting the desktop application. Browser UI
tests do not exercise the native drop handler.

## Controls

Settings apply across managed agents and save immediately. Provider choices are preserved.
A single page groups controls into clear sections:

- **Workload:** parallel tasks and the number of free slots before looking for more work.
- **Planning:** check interval and daily planning-check limit.
- **Approvals:** maximum estimated risk for suggestions, with a reminder that every task needs approval.

All settings use the same 14px text size, with weight and spacing separating the
sections. Only useful field hints and one autosave note remain. There are no
category tabs, disabled approval selector, or account/profile placeholders.
Settings scroll within the window when necessary.

Add projects in Activity opens the native folder picker directly. Existing goals
are preserved; new folders use a default objective to find evidence-backed work.
There is no global on/off, pause, or stop control in the desktop. Opening the app
resumes discovery for already enabled, explicitly scoped projects from older off
or paused setups. Adding a project also enables discovery. A legacy null scope
never opts projects in. The desktop does not repeatedly override an external pause
during polling. Backend/CLI lifecycle commands remain available.

Suggestions still need approval before execution. Independent observed Codex
sessions gate discovery; configured capacity and daily budgets still apply.
Each running task retains **Stop task**. Unconnecting a project stops its work and
removes it from discovery. Claude remains a worker provider. Risk is an assessment,
not a substitute for execution permissions.

Activity opens by default and groups work by project name. A small dot beside the project name shows session presence: green for a live code
session, hollow otherwise, with “Live code session” / “No session” hover labels. Prodex rows show a short
title and elapsed execution time, sorted by latest activity. Expand a task for its
status, approve/reject/stop controls, and session-opening actions. Long reports stay out of the management UI. Planner progress and cooldown
countdowns explain waiting; full folder/session details remain available in the CLI. It never runs shell commands. History scrolls
inside Activity.
Observation failures and inactive projects have explicit waiting states.
Diagnostics also remain available through `prodex status`, `prodex inspect`, and
`prodex events`.
Closing it does not stop the daemon. Authentication is configured outside the GUI.

## Checks

```sh
npm run build
npm run test:ui
cargo check --manifest-path src-tauri/Cargo.toml
```

Playwright tests supply a simulated IPC service and check the fixed-size layout,
legacy activation migration, single-page settings layout, threshold/autonomy/risk persistence, direct folder-picker
invocation, cancellation/errors, duplicate selection and goal preservation, connection loss, failed-save rollback, activity
grouping, observed sessions, task actions, and legacy/null allowlist handling. They do not
invoke models or change a user's running daemon. Install the test browser with
`npx playwright install chromium` if needed.

The folder picker uses the Rust side of [Tauri’s dialog plugin](https://v2.tauri.app/plugin/dialog/). Restart the native desktop process after upgrading to load the picker command; a frontend reload alone is insufficient. Browser tests simulate the OS dialog response.

## Copying a session

Expand a session for **Copy resume command** or **Copy session ID**. The command
changes into its worktree (or project folder) before running `codex resume` or
`claude --resume`. Paths and IDs are shell-quoted for zsh/bash. The app copies text;
paste it into your terminal to continue. It rechecks task state before copying a
resume command, and only offers that command for stopped/completed sessions.
Running sessions still offer the ID, with a reminder to stop before resuming.

A suggestion awaiting approval has no provider session yet, so it offers **Copy
prompt** instead. Copying does not approve, start, stop, or reject a task. Native
clipboard support uses the [Tauri clipboard plugin](https://v2.tauri.app/plugin/clipboard/)
and requires the rebuilt desktop process. Browser tests simulate clipboard writes.

**Open in Terminal** opens a stopped/completed session in macOS Terminal.app,
using the same quoted resume command. macOS may ask for permission to control
Terminal. Copy remains available for Ghostty, Zed, or other terminals. Native
opening rechecks task status and refuses sessions still owned by the worker.

Task cards show a short title and a colored status icon (with hover and accessible
labels). Expand for a PR-style proposed change, implementation approach, and
verification checks, with task actions above the description. New planner output
stores the brief separately from agent instructions; older tasks show their saved
instructions. Completion status does not claim that proposed verification passed.

## Organizing Activity

Project headers collapse their contents. Drag the dotted handle to reorder projects,
or focus it and use the up/down arrow keys. Order and collapsed state are saved on
this desktop. Completed and rejected ideas are hidden by default behind the history
button at the bottom of each project; failures remain visible.

**Check now** beside the countdown skips the planning cooldown only. It still needs
a live Codex session in an allowed project, available capacity, no pending proposals,
and remaining daily budget. It does not approve or launch suggested tasks.

The × button opens **Unconnect project** confirmation. Confirming revokes permission,
cancels this project's planning/pending work, and stops its owned workers. Files,
worktrees and history remain. Other projects are unaffected. Add projects reconnects
the folder and restores its history. Resolve any worker recovery first.

These controls require service protocol 8; older daemons display restart guidance.

Planning defaults to a one-minute interval when eligible, with a service-wide limit
of 1,440 checks per UTC day. A daily cap is labeled separately with its reset countdown;
Check now is hidden while that cap is reached. Waiting for approval, capacity, live
Codex presence, or an already running planner can postpone the next check.

Settings exposes **Check interval** (30 seconds through 30 minutes) and a positive
whole-number **Daily check limit**. Both save immediately. Existing custom intervals
set from the CLI are preserved and shown in the selector.

Unsuccessful attempts stay in **Needs retry** with a toast, a concise reason, and
**Try again** / **Reject** actions. Retrying is explicit and rechecks project policy,
observation, capacity and budgets. A retry starts a fresh attempt/worktree; previous
worktrees and provider sessions are retained in the task's `attempts` history
(available through CLI inspect). Fix setup errors such as a missing Git repository
with an initial commit before retrying. Retry history is never automatically merged.


For an allowed project with no Git repository or initial commit, Activity shows a
“Create a Git repository and initial commit” suggestion first. Approval starts the
selected provider directly in that folder. Other coding cards wait for verified
Git setup before offering Approve/Try again; they always execute in worktrees.
Setup can be rejected or retried like other tasks.


## Workspaces and completion actions

Open **Workspaces** in Settings to choose an individual project and switch
between the default isolated worktrees and main-folder coding. Main-folder mode
runs one Prodex coding task at a time; independent sessions share those files.

Open **When finished** to order **Merge locally** and **Open in your preferred app**. The first applicable action is highlighted. There is no PR action.

**Merge locally** creates a managed background integration job using the task's provider. It reviews/tests the work, commits task changes and integrates locally. The card shows queued/running/stopped/needs-attention progress and supports Stop/retry. The coding result stays visible until Prodex independently verifies a clean committed task result in the pinned destination branch's history, unchanged destination history, and preserved unrelated staged/unstaged/untracked edits. Only then does one database transaction finish the merge job and mark the source task integrated, releasing dependencies and moving it into completed history. A completed agent turn alone is insufficient. Already-merged committed results can be recognized when the action is selected.

Main-folder results retain **Mark reviewed** because their files are already applied. Merge jobs share concurrency/budget controls and run exclusively against other managed project workers. After a crash they require recovery acknowledgement, like coding workers. Independent human/agent edits cannot be locked; changes during integration cause verification to stop rather than silently mark success. No push, PR or deployment is performed.

### Open sessions in your preferred app

The arrow beside **Open in …** lists supported apps installed in `/Applications`, `~/Applications`, and the macOS utility folders. Selecting an app saves the default on this computer. **Other app…** uses the macOS app picker; custom choices are remembered and checked again on startup. Missing or provider-incompatible preferences fall back to an available destination without overwriting the saved preference.

- Terminal, Ghostty and iTerm run the complete, quoted resume command.
- Codex opens the existing thread through `codex://threads/<id>` when Codex.app is installed. Only Codex tasks offer it.
- Claude Code opens Claude.app and copies the Terminal command; use Desktop's `/resume` picker to select the CLI session. Prodex does not invent a Claude session URL or silently migrate API-backed sessions. Desktop authentication/session availability still applies.
- Zed, VS Code and Cursor open the actual project/worktree folder and copy the resume command for their integrated terminal.
- Other detected terminals (cmux, Warp, WezTerm, kitty, Alacritty) and manually selected apps open with the resume command copied. They do not yet have automatic command-injection adapters.

**Copy command** always remains available. Opening an app is user-triggered and does not stop a managed worker. Discovery currently targets macOS. No keystrokes, editor extensions or project configuration are injected.

References: [T3 Code editor preferences](https://github.com/pingdotgg/t3code/blob/main/apps/web/src/editorPreferences.ts), [T3 Code app discovery](https://github.com/pingdotgg/t3code/blob/main/packages/shared/src/editor.ts), [Codex deep links](https://learn.chatgpt.com/docs/reference/commands), [Claude Desktop handoff](https://code.claude.com/docs/en/desktop), [Ghostty command options](https://ghostty.org/docs/config/reference).


### Background integration permissions

Codex merge workers use `--approve-for-me`, the workspace sandbox, and `--add-dir` for the original project. Claude merge workers use `--permission-mode auto`, the existing restricted mode and an explicit destination directory; shell actions go through its classifier rather than a blanket Bash allowance. Provider/account/managed-policy restrictions can still block a merge and are reported for retry. Neither provider uses a sandbox/approval bypass.

The native Open action only opens/resumes sessions. Merge is owned by the daemon and does not open a terminal. Historical manual integration acknowledgements are retained in the database; new desktop merges require Git verification.
