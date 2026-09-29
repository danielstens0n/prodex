# Architecture and implementation boundaries

This document describes the initial Rust implementation. `docs/release-readiness.md` tracks public release gates; a compiled module is not evidence that a real-provider or packaged-desktop acceptance gate passed.

## Ownership and contracts

`prodex-core` owns domain records, SQLite persistence, launch policy, planner encoding/decoding, provider subprocesses, workspace creation, and local IPC. `prodex` is a thin command client plus a daemon entry point. `apps/desktop` is a Tauri/TypeScript client. Neither terminal UI nor desktop window owns an agent's lifecycle; the independent daemon does.

| Contract | Responsibility |
| --- | --- |
| `Project` | Canonical project directory, approved objective/version, enabled state |
| `Settings` | Service/project worker ceilings, pause, timeouts, daily start limits, planner settings |
| `TaskProposal` | Concrete task, rationale, completion criteria, dependencies, expected paths, mode, provider, objective version |
| `TaskRecord` | Automatic/manual origin (persisted), Prodex identity, provider session identity, execution status, separate review status, worktree/branch/base commit, owned process ID, summary |
| `Observation` / `ObservedSession` | Ephemeral live Codex terminal presence, process identity, canonical cwd/project, freshness and source; never persisted as proof of activity |
| `RunSpec` | One supervised provider turn: provider, working directory, prompt, mode, optional prior session ID |
| `ProviderOutput` | Normalized session identity, assistant text, terminal result, errors, and USD usage when supplied |
| `WorkerEvent` | Process started, normalized output, and final result combining protocol completion with actual exit status |
| `Request` / `Response` | JSON control protocol over a local Unix socket; shared by CLI and desktop |

The shared protocol version is currently 8, including explicit folder permission and live observation. Schema version 3 records launches by task and attempt; the version 2 migration migrates legacy all-project scope to an empty selection; automatic work is disabled and paused for that migration. The desktop disables controls and requests a service restart when versions differ. Providers are concrete modules behind dispatch functions, not yet a broad capability trait. Adapter support for a resume argument is distinct from an implemented UI/control workflow to resume an existing task.

## Serialized decisions, parallel workers

One service actor owns mutation and scheduling. It rechecks launch eligibility and reserves a task before considering the next task, preventing simultaneous completion events from overbooking capacity. SQLite preserves tasks, settings, projects, events, and start accounting. A file lock prevents two daemon instances from owning the same state directory. The directory/socket permissions restrict other OS users; IPC is not designed to defend against malicious processes running as the same user.

Worker processes run concurrently up to both configured ceilings, initially three. Starting, running, stopping, and recovery-required tasks consume slots. Queued tasks do not consume a slot. User sessions running outside Prodex are not counted or controlled.

`policy::validate_proposal` checks fields, configured project/objective, dependency identities, lexical relative paths, and normalized-prompt deduplication, including rejected work. `policy::eligible` checks observation and explicit folder permission for automatically proposed tasks, live pause state, status, budgets, capacity, dependency completion/integration, and overlapping edit scopes. Empty edit scope conservatively means the whole project; a directory scope overlaps its descendants. Scope comparisons are conservative about case. These are scheduling hints, not access-control rules or semantic proofs that tasks are independent.

The planner's input includes the objective and a bounded recent-task snapshot, with active tasks prioritized. It researches through a read-only provider run. Its accepted output is strict JSON containing a proposal list (possibly empty) and an optional bounded project notebook update. At most three proposals and 64 KiB output are accepted. The parser injects project, objective version, and worker provider from coordinator-owned state, rejecting model-supplied substitutes. Parsing does not approve or launch work. Fresh service policy checks remain necessary after a planning run, since capacity, objective, and pause state can change while it runs.

Planning waits for `planning_free_slots` free worker slots both globally and in the project (default two). The effective threshold is capped by the smaller configured concurrency limit so a one-worker setup can still plan. Explicit planning requests obey the same threshold. `max_proposal_risk` defaults to medium: planner proposals assessed above the selected low/medium/high ceiling are discarded before tasks are created. Older proposals and omitted planner risk fields default to medium. Risk is a model assessment, not an access-control guarantee. All coding tasks require approval; legacy read-only auto-approval has no effect.

## Project activation and observation

The global switch never grants folder permission. Project registration with
`enabled:true` explicitly allows that canonical folder; selection changes persist
atomically with project flags. An empty selection means no automatic work.

A separate asynchronous scan observes independent interactive Codex terminal
processes every five seconds, with a ten-second overall deadline. Observations
expire after fifteen seconds and are not restored from SQLite on restart. Matching
uses canonical paths and the most specific allowed root. Prodex-owned processes,
noninteractive executions, and worktree roots are excluded. See the Codex evidence
document for platform/probe limits. A process identity is not a provider thread ID.

Automatic planning requires On, an allowed project, and live observation. The actor
rechecks after planning and before provider launch, including asynchronous worktree
preparation. Automatic origin persists with each task, so approval cannot bypass
observation. Session loss cancels automatic research and suspends new automatic
workers but does not interrupt existing workers. Explicit CLI plan/submit requests
remain available without observation; normal project/pause/budget checks still apply.

The planner receives session-presence metadata, not the user's conversation. It is
instructed not to infer the user's active task from presence alone. Rich main-session
context and repository event subscriptions remain separate work.

## Provider boundary

Both providers are implemented as supervised local CLI processes with structured JSON streams. The initial Codex implementation uses `codex exec --json`; the researched App Server path remains an extension point rather than a runtime dependency. Claude uses API-backed `claude --bare --print --output-format stream-json` with explicit file tools. See [Codex evidence](codex-integration.md) and [Claude evidence](claude-integration.md).

Prompts go through stdin, not shell interpolation or process arguments. The runner bounds event frames, drains stderr separately, recognizes successful terminal events, and also requires successful process exit. Invalid protocol events and missing completion are failures. Provider stderr is drained without persistence to avoid retaining incidental sensitive diagnostics. Provider assistant text and selected result metadata are persisted, so the state directory still contains potentially private project content.

Codex is requested to use read-only or workspace-write sandboxing with approvals disabled; a read-only Codex turn may still invoke sandboxed shell tools and read beyond the named task's expected files. Claude's initial read-only profile exposes Read/Glob/Grep; approved editing additionally exposes Edit/Write, without Bash. These are different capabilities, not interchangeable claims of isolation. Neither profile is a guarantee of per-file scope confinement enforced by Prodex.

Codex ignores user configuration and execpolicy rules in the initial command. That does not establish total isolation from project/managed configuration, hooks, plugins, or MCP. Official [hook documentation](https://learn.chatgpt.com/docs/hooks) describes project, user, plugin, and managed hook sources, with separate trust handling. Never enable hook-trust or sandbox bypass flags to make automation work. Treat configured projects and provider configuration as trusted until stronger isolation has been validated.

Claude's bare/restricted profile avoids ambient user/project customizations and requires the API route; its key remains in the process environment. Usage estimates are informational. Daily worker/planner start counts and timeouts are enforceable bounds, but this stage does not offer a cross-provider dollar spending cap. A Claude resumed conversation can report cumulative cost; adding it to previous totals would double count.

## Workspace and lifecycle

Editing work receives a dedicated branch/worktree based on a recorded commit. The primary checkout's uncommitted changes are not copied. Read-only work reads the project checkout, which can change during research; it is not a consistent filesystem snapshot. Worktrees do not isolate credentials, ports, services, Git configuration, or arbitrary processes.

Execution and review are separate. A successful editing turn still awaits review, and dependency checks require integration where applicable. Automatic merging, pushing, publishing, and deployment are outside this implementation. Review/integration APIs must be completed before dependent editing workflows can progress entirely through Prodex.

Pause prevents new launches; current workers continue. Stop interrupts an owned worker and preserves its workspace/history. Stop-all pauses scheduling first and requests interruption of managed work. Service shutdown is separate from closing the desktop window. After an unclean restart, previously active workers become recovery-required rather than silently retried. Prodex does not signal a stale persisted PID: PID reuse could target an unrelated process. Explicit recovery acknowledgement releases the blocked task without automatically restarting it.

## What the first slice does not establish

- Real-provider end-to-end acceptance: command/help probes and synthetic protocol tests do not prove successful authenticated work, permission boundaries, or session resume.
- Native app attachment: a persisted session is not proof of automatic desktop sidebar visibility, live mirroring, or safe takeover. CLI fallback exposes results/session IDs; native handoff needs its own capability and acceptance test.
- Main-session contents and adoption: live interactive Codex terminal presence can activate a project, but Prodex does not read/control its conversation. Desktop/IDE-only and remote discovery, Claude-triggered activation, and rich milestone triggers remain unsupported.
- Full proposal semantics: lexical scope and prompt deduplication cannot prove that a proposed task belongs to the objective or will avoid all conceptual conflicts. Approval remains the default.
- Production recovery and packaging: no multi-machine coordination, Windows transport, guaranteed descendant cleanup across detached process sessions, or fresh-install acceptance is implied.

Future adapters and GUI features should extend these contracts without bypassing service policy. Parallel implementation can proceed behind stable contracts, while migrations, shared types, scheduler integration, and final acceptance remain serialized.


## Goal-driven planning and memory

The planner prioritizes the documented product goal, core user journeys and the
highest-impact gap. It adjusts its emphasis to maturity without changing the
approved goal; cleanup requires a demonstrated product benefit. User-facing
briefs name understandable outcomes while worker instructions carry technical
details.

Each canonical project has a `ProjectNotes` value in SQLite's `meta` table under
`project_notes:<absolute project path>`, with its objective version and up to
12,000 UTF-8 bytes of text. A planner reads it in context and returns its full
replacement as `project_notes`. The coordinator persists it only after validating
the whole response and rechecking project version, activation and cancellation.
Abstention can update memory; omission preserves existing notes. Invalid, failed,
cancelled and stale passes cannot overwrite notes. They survive daemon restarts
and do not create files or edits in the user's repository.

Notes summarize evidence, maturity, known gaps, completed/integrated work,
constraints, rejected ideas and uncertainties. Task history remains authoritative;
up to 32 recent rejected ideas are supplied separately so active work cannot
crowd all feedback out of context. No rejection-reason input exists yet, so the
planner must not infer one. Notes cannot grant permissions or override the goal.
They are currently internal planner memory, without a desktop editing surface.


Projects persist `use_worktrees` (default true). The coordinator chooses `edit` or
`edit_in_place` at launch from this setting; untrusted submissions cannot request
direct execution modes. Main-folder tasks hold an exclusive Prodex worker slot
within the project and receive workspace-write/file-edit permissions, not the Git
setup profile. Workspace changes are refused while managed work is active.
Completed editing results require explicit manual review/integration
acknowledgement before dependencies are eligible. Terminal handoffs do not change
review state and do not prove that integration occurred.


## Background integration

`merge` creates a coordinator-owned `TaskMode::Merge` job linked to one succeeded worktree task. The same worker lifecycle supplies launch accounting, stop, timeout, retry and recovery. Merge jobs serialize managed project work. A prelaunch Git snapshot records destination branch/HEAD and local changes in SQLite metadata. After worker success a separate read-only verification checks ancestry, worktree cleanliness, destination identity/history and local-change preservation. Only verified success atomically updates the merge job and source review to Integrated. The UI hides internal merge rows and shows progress on the coding card. The previous manual acknowledgement request remains for compatibility, but is not used by the new desktop merge action.
