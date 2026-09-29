# Codex integration evidence

Probe date: 2026-09-29. Installed binary: `codex-cli 0.158.0` on macOS.

## Evidence and limits

Inspected `codex --version`, `codex app-server --help`, `codex exec --help`,
`codex exec resume --help`, and `codex resume --help`. Generated the installed
protocol using `codex app-server generate-json-schema --out /private/tmp/prodex-codex-schema`.
These commands succeeded; the restricted environment emitted a harmless PATH-alias
creation warning. No model turn, account change, desktop automation, or paid request
was performed. Runtime protocol and native app interoperability tests remain pending.

The official [App Server documentation](https://learn.chatgpt.com/docs/app-server)
documents initialization followed by thread and turn operations, with JSON messages
over stdio. It also documents API-key authentication and Codex-managed ChatGPT login;
external ChatGPT-token handling is experimental. Prodex should let Codex manage its
own login and token storage, or use the documented API-key route. Never copy tokens
into Prodex task records. Documentation alone does not establish desktop sidebar
discovery for third-party-created threads. That requires an explicit integration test.

## Preferred transport: owned App Server process

The first implementation slice uses the supervised `codex exec --json` fallback
below. App Server remains the planned richer transport; it is not implemented yet.
The CLI adapter explicitly disables daemon sharing, ignores user config and rules,
sets `never` approvals and the requested sandbox, and accepts prompts only on stdin.
These controls do not assert that project hooks/configuration are fully isolated.
The root `--no-daemon --ask-for-approval never` placement before `exec` and parent
`exec --sandbox ... --json` placement before `resume` were checked with installed
CLI help parsing. Provider runtime execution remains a separate acceptance gate.

Use `tokio::process::Command` with argument arrays, piped stdin/stdout, and separately
drained stderr. Start `codex app-server --listen stdio://`. Do not scrape the TUI.
Correlate response IDs and dispatch notifications and server-initiated requests
independently. Use a bounded maximum message size and timeouts.

The installed schema verifies these request contracts:

| Operation | Method | Key fields |
| --- | --- | --- |
| Initialize | `initialize` | Required `clientInfo`; optional `capabilities` |
| Create persistent conversation | `thread/start` | `cwd`, `sandbox`, `approvalPolicy`, `ephemeral: false` |
| Continue stored conversation | `thread/resume` | Required `threadId`; optional permission overrides |
| Run input | `turn/start` | Required `threadId`, `input`; optional `outputSchema` |
| Cancel active turn | `turn/interrupt` | Required `threadId`, `turnId` |
| Discover stored conversations | `thread/list` | Optional `cwd`, `sourceKinds`, `originators`, cursor |

After `initialize` succeeds, send `initialized` as a notification before other
operations. A minimal initial exchange is:

```json
{"id":1,"method":"initialize","params":{"clientInfo":{"name":"prodex","version":"0.1.0"}}}
{"method":"initialized","params":{}}
{"id":2,"method":"thread/start","params":{"cwd":"/absolute/worktree","sandbox":"read-only","approvalPolicy":"never","ephemeral":false}}
{"id":3,"method":"turn/start","params":{"threadId":"<returned-id>","input":[{"type":"text","text":"<bounded task>"}]}}
```

Wait for each dependent response. The installed `SandboxMode` enum uses
`read-only`, `workspace-write`, and `danger-full-access`. In particular, do not copy
the differently spelled `workspaceWrite` example from the documentation into this
version's `thread/start`. `turn/start.sandboxPolicy` is a separate structured type.
Pin the supported CLI version and regenerate fixtures when upgrading.

Normalize installed-schema notifications `thread/started`, `turn/started`,
`item/agentMessage/delta`, `item/started`, `item/completed`, `turn/diff/updated`,
`thread/tokenUsage/updated`, and `turn/completed`. Preserve thread, turn, and item
identifiers. Acknowledgment of interruption is not completion: await the terminal
turn notification, or report connection loss/unknown state. Token usage is not a
guaranteed monetary cost.

Server requests include `item/commandExecution/requestApproval`,
`item/fileChange/requestApproval`, `item/permissions/requestApproval`, and
`item/tool/requestUserInput`. Do not interpret these as notifications or automatically
approve them. Until an approval UI exists, choose an explicit restrictive policy and
return unsupported input requests honestly. Research should use read-only mode;
editing requires an approved worktree and explicit workspace-write policy. Never
use the bypass flag as an automation shortcut. Project hooks, MCP configuration,
and inherited provider settings also need consideration before executing real work.

## CLI fallback and manual continuation

Installed CLI help supports structured one-shot runs:

```text
codex exec --json --sandbox read-only --cd <absolute-worktree> -
codex exec --sandbox read-only resume --json <exact-session-id> -
codex resume <exact-session-id>
```

Pass the prompt on stdin and each argument separately. Keep stderr separate from
JSONL. Persist the session ID from the structured stream, require a successful
terminal result as well as process status, and never resume using `--last` in a
concurrent coordinator. Omit `--ephemeral` when continuation is required. CLI-only
interruption requires supervised process signaling; signaling success alone does not
prove that all tool descendants stopped. Its behavior needs an integration test.

`codex resume` supports `--include-non-interactive` for its picker and `--remote`
with Unix/WebSocket endpoints in this installed version. These are useful future
integration candidates, not proof of native desktop visibility. Only offer manual
continuation after the managed worker is stopped or complete, to avoid two writers.

## Observation, adoption, and app visibility

Prodex can consume events on a connection it owns. Persisted conversation identity
must be separate from active worker identity. A stored thread ID is not evidence
that its worker is running. `thread/list` filters also mean a UI may hide sessions
even when storage is shared.

Adoption of arbitrary already-running CLI/Desktop sessions is unsupported in the
initial contract. The local CLI advertises daemon/proxy and remote transports, but
their presence alone does not validate ownership, subscription, or safe control of
an existing session. Do not read private databases or start another worker merely
to simulate attachment. Keep `native_app_open`, `live_attach`, and
`adopt_existing_worker` false until isolated end-to-end probes pass.

## Minimal Rust boundary

Use an adapter that returns a running handle rather than conflating an asynchronous
worker with its durable conversation:

```text
capabilities() -> start, stream, continue, interrupt, usage, attach flags
start(TaskLaunch) -> RunningHandle { provider_session_id, worker_id, events }
continue(SessionRef, TaskLaunch) -> RunningHandle
interrupt(WorkerRef) -> request acknowledgment (completion arrives separately)
reconcile(SessionRef, WorkerRef) -> active | idle | terminal | unknown
open(SessionRef) -> supported launch instructions | unsupported
```

`TaskLaunch` needs an absolute cwd, prompt, allowed execution mode, optional model,
and time/budget constraints. `ProviderEvent` needs started/progress/input-needed/
completed/failed/interrupted/connection-lost plus optional usage. The coordinator
owns capacity, objective versions, worktrees, persistence, and launch authorization.
The adapter must not invent a successful completion after EOF or restart.

## Remaining acceptance work

- Run an isolated read-only turn and verify streamed output and persisted identity.
- Continue that exact conversation and test interrupt while a turn is active.
- Verify disconnect handling and reconciliation without duplicate execution.
- Test permission requests and unsupported input handling.
- Test native desktop discovery, display, and safe handoff separately; retain CLI
  continuation as the supported fallback until then.

## Implemented MVP observation: local interactive terminals

The September 29 implementation observes **live local interactive Codex CLI
processes**, independently of their terminal host (including Zed's terminal and
Ghostty). It does not attach, adopt, resume, send input, or read their transcripts.
This support is narrower than discovery of every Codex Desktop/IDE conversation.

The observer uses read-only `ps` process snapshots and `lsof` working directories.
It requires the current user's process, a live TTY, a `codex` executable name
checked independently of argv, and recognized interactive arguments. It excludes
`exec`, `review`, `app-server`, remote connections, unknown arguments, ambiguous
positional prompts, relative `--cd`, and `resume --last`. Explicit `resume <UUID>`
is supported unless the ID belongs to a Prodex worker/planner. `--worktree` is
currently excluded because the process's initial cwd does not establish the
session's managed worktree. Absolute `--cd` is supported. A bare interactive TUI
counts as present even while idle or at its initial prompt; this is process
presence, not evidence that a model turn is running.

Prodex process descendants, known managed session IDs, and managed worktree roots
are excluded. Matching canonicalizes both project roots and observed directories,
uses path components rather than string prefixes, and chooses the deepest allowed
project root. A second snapshot checks PID, start time, owner, parent, arguments,
and TTY again before publication. Session IDs are clearly labeled synthetic
`codex-process:<pid>:<start-time>` identities, never provider thread IDs.

No historical files, private databases, credentials, or process environments are
read. Old conversations therefore cannot activate a project. OS visibility errors
and unsupported platforms fail closed with an unavailable observation state;
unrecognized invocations are simply not observed. macOS was verified; Linux uses
the same `ps`/`lsof` interface but still needs a live platform acceptance test.
`ps` and `lsof` must be installed and visible on the service PATH. Each probe has a
three-second timeout and kills its subprocess if cancelled. The coordinator also
bounds the whole scan and expires stale observations.

Installed CLI `codex-cli 0.158.0` help advertises `app-server proxy` to the running
control socket. A read-only initialize/loaded-thread-list probe did not establish
a working response in this environment; it is not the implemented transport.
A newly started private App Server would not establish visibility into other
instances. No daemon was started/restarted and no model turn was invoked during
observation research.

Validation: fixture tests cover interactive/noninteractive forms, absent TTY,
canonical-root matching, sibling prefixes, excluded worktrees, ancestry, and PID
reuse. An opt-in OS acceptance test detected two independent interactive Codex
terminals in this project on macOS without changing service settings or starting
work:

```sh
PRODEX_OBSERVATION_TEST_PROJECT=/absolute/project \
  cargo test -p prodex-core live_terminal_observation --lib -- --ignored --nocapture
```

## First real planner result

On September 29, the user's enabled daemon ran a real Codex read-only planning pass
and persisted one proposal awaiting approval (events 30–32, task
`e002f39b-479b-44b4-a3cd-efb791df4ae7`). An earlier pass was interrupted by Off;
re-enabling obeyed the five-minute cooldown. During the same session the user approved that proposal; the Codex read-only worker
completed successfully and stored its report and provider session ID. A second
proposal awaits approval. This validates real planning and read-only execution,
not editing, continuation, or native app handoff. The assistant did not approve
proposals as part of UI diagnostics.


### Git setup exception

With worktrees enabled, coordinator-owned `initialize_repository` tasks alone run in the original project
folder, after approval and a fresh missing-repository/unborn-HEAD check. The exec
command uses `--skip-git-repo-check` and this command-scoped permission profile:

```toml
default_permissions = "prodex_git_setup"
[permissions.prodex_git_setup]
extends = ":workspace"
[permissions.prodex_git_setup.filesystem.":workspace_roots"]
".git" = "write"
```

Pass the profile as a single inline TOML table in `-c`; dotted CLI keys containing
`.git` did not deserialize correctly in the installed CLI. Normal coding keeps
its existing workspace sandbox/worktree path. The coordinator verifies HEAD after
setup completion before releasing dependencies. Tested locally with Codex 0.158
using `codex sandbox` and a temporary Git fixture; authenticated setup remains a
release check. See the official [permission configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).


Projects can now explicitly disable worktrees. Normal coding then uses
`edit_in_place`, the existing workspace-write sandbox and `--skip-git-repo-check`,
without the Git-metadata write override. This is a project setting, never an
untrusted planner-selected execution mode. Review/merge and PR completion actions
resume the recorded session interactively with a positional prompt, supported by
the installed `codex resume --help`. Those handoffs request user review and
confirmation before integration/publication; they do not perform Git operations
in Prodex itself.
