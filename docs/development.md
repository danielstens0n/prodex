# Running the development build

This is an initial working slice, not a completed MVP. The service, CLI, and GUI
are usable with simulated workers; Codex and Claude process adapters are included.
Real Codex planning and a user-approved read-only worker have completed successfully.
Claude execution, editing/continuation, and native-app handoff still need validation. Independent interactive Codex terminal sessions provide project activation;
conversation contents and desktop/IDE-only sessions are not observed.

## Build and try without model calls

Requirements: Rust/Cargo, Git for editing tasks, and macOS or another Unix system.
The desktop additionally requires Node 22.12+ and Tauri's platform prerequisites.

From the repository root:

```sh
cargo build --workspace
export PRODEX_STATE_DIR=/tmp/prodex-demo
./target/debug/prodex start
./target/debug/prodex project --path "$PWD" --objective "Implement and validate the Prodex coordinator"
./target/debug/prodex configure --provider mock --planner-provider mock
./target/debug/prodex submit "Demonstrate the task lifecycle" --provider mock --approve
./target/debug/prodex status
./target/debug/prodex events
./target/debug/prodex plan
```

Mock tasks take about three seconds and never call a model or edit files. The mock
planner returns an empty proposal list: capacity does not force work. A prompt
containing `mock:fail` exercises the failure path.

`prodex start` detaches the daemon and writes diagnostics to `daemon.log` in the
state directory. `prodex daemon` runs in the foreground instead. Commands must use
the same state directory. Without an override, it is `$HOME/.local/state/prodex`.
Do not use an existing project directory as the state directory; it is private
service storage and its permissions are set to 0700.

## Desktop

Keep the same `PRODEX_STATE_DIR` environment variable, then:

```sh
cd apps/desktop
npm ci
npm run tauri dev
```

The window connects to the existing daemon. Activity shows work grouped by project,
observed Codex presence, pending approvals, and results. Settings contains thread
count, free-slot threshold, autonomy, and risk. Click Add projects beneath the
Activity description to select folders in the native file dialog; selection
immediately allows them, with no setup form.
On/off, Pause, and Stop remain accessible across tabs. Closing the window leaves the
daemon running. It does not install or automatically start the service. See
[desktop setup](../apps/desktop/README.md).

Protocol 3 requires the updated daemon as well as the updated UI. After rebuilding,
stop the old service with `prodex shutdown` and start the new binary. Shutdown stops
managed workers and preserves their work. Legacy “All projects” settings migrate to
an empty selection with automatic work off; explicitly select folders again.

If the shell resolves an older Node despite Volta being installed, use
`volta run --node 24.20.0 --npm 11.19.0 npm run tauri dev` from `apps/desktop`.

## Use a real provider

Install a compatible `codex` or `claude` CLI on the daemon's PATH. The interfaces
were checked against Codex 0.158.0 and Claude Code 2.1.269. Newer or older versions
may need adapter changes.

- Codex uses the CLI's existing supported authentication and explicit sandbox
  settings. Prodex does not read or copy the user's credential files.
- Claude uses the API-backed route. Set `ANTHROPIC_API_KEY` in the daemon's
  environment before starting it. The GUI never requests or stores this key.
  Restarting only the GUI does not update the daemon's environment.

```sh
./target/debug/prodex submit "Fix a documented parser bug and add a regression test" --provider codex --approve
./target/debug/prodex submit "Implement a bounded improvement from the README" --provider claude --approve
```

`submit` without `--approve` creates a proposal awaiting approval. Codex and Claude tasks always use edit mode; `--edit` remains accepted for compatibility. Standalone research tasks are rejected. Editing creates an external Git worktree
under the service state directory. Normal coding requires an initial Git commit. For an allowed, observed project without Git or without its first commit, Prodex automatically proposes “Create a Git repository and initial commit.” Approval runs this dedicated setup session directly in the project, with the selected Codex or Claude worker. No other coding task can use this exception. Setup verifies HEAD before releasing dependent tasks, respects rejection, and can be retried. Existing Git identity is required; the agent must not invent one. Broken Git metadata is an inspection error, not permission to reinitialize.

Read-only does not mean identical tools across providers: Codex can use its
read-only sandboxed shell, while Claude currently receives only Read/Glob/Grep.
Claude editing adds Edit/Write but does not permit Bash, so it cannot run a test
suite itself in this initial profile. Record that limitation when reviewing its
results. Expected-file scopes are scheduling hints, not filesystem permissions.

## Planning and controls

Configure the planner and worker providers independently:

```sh
./target/debug/prodex configure --provider codex --planner-provider claude
./target/debug/prodex plan --project "$PWD"
./target/debug/prodex configure --planning-enabled true
```

Automatic planning defaults off. Add and explicitly allow each project folder in
Activity using Add projects (or use `prodex project --path ... --objective ...`), then turn On.
New picker-added folders use a default objective based on documented repository
goals; existing objectives are preserved. You can refine an objective through the CLI. There
is no implicit “All projects” permission. On waits for an independent interactive
Codex CLI session in an allowed folder. Either provider can plan/execute work;
a Claude-only session does not activate automatic work.

The observer polls live local terminal processes every five seconds. A session
already running when a folder is allowed can be detected. It matches canonical
working directories, including descendants, to the most specific allowed root.
Prodex workers, planners, noninteractive Codex executions, and external worktrees
are excluded. Saved transcripts are never activation evidence. OS process visibility
is required; unsupported/failed observation blocks new automatic work. Observations
expire after 15 seconds. This detects presence, not the contents of the main chat,
and does not claim discovery of desktop-only, IDE app-server, or remote sessions.

Session loss suspends new automatic planning/launches, cancels active automatic
research, and lets existing workers finish. Queued automatic proposals retain their
observation requirement even after approval. The gate is rechecked after research
and worktree preparation. Explicit CLI `plan`/`submit` requests can run without an
observed session in an allowed folder; they still obey pause, capacity, and budgets.

The default refill threshold is two free slots, capped by concurrency. Suggestions
above the selected risk ceiling are discarded. The service permits one planner at
a time, with a one-minute per-project cooldown and up to 1,440 planning passes per UTC day across the service.
Pending proposals must be handled before another planning pass. Timers and task
completions cannot activate a folder without a qualifying observed session.

Proposals are validated against the current objective version, duplicates, and
dependencies. Every planned task requires approval. The legacy
`auto_approve_read_only` field is retained for compatibility but has no effect. Default limits are three workers globally
and per project, twenty worker starts per UTC day, and ten minutes per worker.
Planning passes have a five-minute maximum. These are count/time limits, **not a
monetary spending cap**. Provider billing estimates are not yet unified.

```sh
./target/debug/prodex pause             # Cancel research, prevent launches; workers continue
./target/debug/prodex resume
./target/debug/prodex stop TASK_ID
./target/debug/prodex stop --all        # Pause and stop managed work, including queued proposals
./target/debug/prodex inspect TASK_ID
./target/debug/prodex open TASK_ID      # Print a native resume command for a stopped/completed task
./target/debug/prodex shutdown          # Stop managed work, preserve files, stop service
```

`open` prints commands rather than opening native tabs. It refuses active or
unreconciled workers to avoid competing turns. Native app discovery is not
guaranteed. Adapter-level resume exists, but continuing a completed task through
the daemon is not implemented yet.

Worktrees remain after failure, stop, or completion. Review/integration controls
and cleanup are still pending. Editing dependencies remain blocked until an
integration workflow is implemented; the service never silently merges work.

After a crash, tasks previously running become `recovery_required`, retain their
session/worktree identity, consume capacity, and pause scheduling. Confirm the
old worker is stopped before using:

```sh
./target/debug/prodex resolve-recovery TASK_ID --confirmed-stopped
./target/debug/prodex resume
```

Prodex never kills a PID loaded from the database because it may have been reused.
Interrupted planner recovery also pauses scheduling and records an event; verify
that old planner processes are stopped before resuming.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/desktop
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
```

Service integration tests use temporary state directories, mock workers, and real
Unix sockets. They need permission to bind a local Unix socket in sandboxed test
environments. Worktree tests use disposable Git repositories. The tests do not
invoke authenticated model sessions.

## Remaining release gates

- Real-provider lifecycle, interruption, permissions, and billing validation.
- Main-conversation content/milestones, desktop/IDE-only observation, and session adoption.
- Desktop session visibility and supported handoff testing.
- Review, integration, cleanup, richer result/diff viewing, and managed continuation.
- Monetary budgets, event retention/pagination, deeper recovery/fault injection.
- GUI runtime smoke test, installation, bundles, and cross-platform verification.

Track implementation milestones in [plan.md](../plan.md).

Activity shows one live-Codex indicator per project, compact title/runtime rows, and
planning progress or the next-check countdown. Expanded rows hide reports and contain session-opening actions and
approval/stop controls. A stop/start does not bypass the one-minute planner cooldown.
Execution durations use recorded worker start times; older records without start
times show their status instead of an estimated runtime.


Workspace behavior is configurable per project under Settings → Workspaces.
Worktrees remain the default. Main-folder coding needs no Git setup and runs one
Prodex task at a time directly among existing files. Completed results remain
visible until reviewed/integrated; completion-action order is configurable under
Settings → When finished. Merge and PR actions continue the agent session in
Terminal for review and confirmation. Integration acknowledgement is manual, not
an automated Git verification. See [workspace details](workspaces.md).


### Provider diagnostics

Use `./target/debug/prodex events --follow` to watch persisted worker/planner debug events (or `events --after SEQUENCE` for a specific interval). Worker events carry the task ID; planner diagnostics include the planning-run ID. Logs include provider/mode, working folder, timeout, PID/session, large stdout message sizes, stream totals, exit status, completion marker, elapsed time and stop reason. Debug events do not reorder task cards. Raw stderr, tool results, prompts and environment variables are not added to diagnostic events; existing agent-message events are unchanged.

Provider stdout JSONL accepts individual events up to 16 MiB, independently of the 1 MiB local IPC limit. The raw-frame queue holds at most four entries. Larger events fail explicitly instead of being silently dropped (which could hide completion/errors). Stderr is drained without line buffering or a line-size limit. A large tool event alone is not a failure. Git setup asks the agent to exclude build/dependency directories and bound file listings.
