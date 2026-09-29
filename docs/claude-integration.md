# Claude Code integration probe

P0-A evidence, 2026-09-29. Local binary: **Claude Code 2.1.269**, confirmed with `claude --version` and `claude --help`. No model request, credential inspection, desktop handoff, or paid run was performed. Capability availability is established from local help and official documentation; real-provider acceptance remains a later gate.

## Adapter decision

Use a Rust-supervised CLI child, one process per managed turn, with newline-delimited JSON output. Prodex owns the background lifecycle. Do not use Claude's native `--bg` mode for this adapter: the documented programmatic mode rejects combining it with `-p`. Native background sessions can attach in a terminal, but that is a different execution path and does not establish structured live attachment to our print-mode process.

The CLI avoids introducing a Python/TypeScript SDK sidecar. Rust should pass arguments directly through `tokio::process::Command`, set the worktree as the working directory, pipe the prompt to stdin, and consume stdout/stderr independently. Never construct a shell command from a prompt or project path.

## Locally verified flags

The installed binary advertises:

| Operation | Supported interface / implementation choice |
| --- | --- |
| New conversation | `--print`, optionally `--session-id <uuid>`; save the actual ID reported by the stream |
| Structured observation | `--output-format stream-json --verbose`; optional `--include-partial-messages` |
| Continuation | `--resume <session-id>`; never select a worker using ambiguous `--continue` |
| Persistence | Enabled unless `--no-session-persistence` is supplied; do not supply it |
| Per-run budget | `--max-budget-usd <amount>` for print mode |
| Tool set | `--tools` controls available built-ins; `--allowedTools` grants permissions; these have different meanings |
| Unattended permissions | `--permission-mode dontAsk --permission-prompts none` |
| Isolation from ambient configuration | `--bare`, `--restricted`, `--strict-mcp-config`; explicitly supply needed context |
| Native background session | `--background` / `--bg`, `attach`, `logs`, `stop`; separate from the selected print adapter |

`--bare` says Anthropic authentication comes from `ANTHROPIC_API_KEY` or an explicitly configured `apiKeyHelper`, never subscription OAuth or keychain. `--restricted` says file tools are confined to working directories, ignores user/project/local settings, and excludes command-execution tools unless explicitly supplied through `--tools`. Never opt back into Bash merely to make a task succeed: worktrees do not sandbox arbitrary processes or external effects.

Proposed initial research argument vector (prompt supplied separately on stdin):

```text
--bare --print --output-format stream-json --verbose
--restricted --strict-mcp-config
--permission-mode dontAsk --permission-prompts none
--tools Read,Glob,Grep --allowedTools Read,Glob,Grep
--max-budget-usd <configured-limit>
```

An explicitly approved file-editing task may add `Edit,Write` to both tool lists. Shell execution and interactive approval hosting need separate policy and implementation; neither is required to verify the first restricted process adapter. Absence of an API key is a clear preflight error. Do not log or persist credential values. The service must not treat CLI permission settings as an OS sandbox.

## Stream and interruption contract

The [programmatic execution documentation](https://code.claude.com/docs/en/headless) specifies JSON lines, `system/init` metadata, `stream_event` text deltas, and a terminal `result` carrying session/cost information. Unknown event variants must remain forward-compatible. Permission denials can appear as system events and result metadata. Treat result failure and nonzero process exit as failures; EOF alone is not success. Resumed session cost totals include earlier runs, so accounting must calculate deltas rather than repeatedly adding cumulative totals.

That documentation specifies SIGINT to end a turn; SIGTERM exits 143 and leaves the turn unfinished while terminating active Bash descendants. Proposed supervision policy: SIGINT first, bounded grace period, SIGTERM next, forced process-group termination only if still alive. Persist interruption intent before signalling; distinguish requested interruption, confirmed exit, and loss of observation. Process shutdown does not imply safe session adoption or completed work.

The initial parser should extract session IDs, assistant text, final result text, `is_error`, cost, and permission-denial metadata using tolerant JSON objects. Test truncated streams, malformed lines, result-error with clean exit, and interrupted exit. Real event fixtures must later confirm the chosen mapping against the installed version.

## Authentication and native interfaces

Anthropic's [legal and compliance documentation](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use) directs developers of products/services to API keys or supported cloud-provider credentials, prohibits intermediating Claude.ai credentials, and distinguishes this from users signing into the unmodified binary themselves. Prodex's distributed integration therefore uses the API route. It must not extract subscription tokens or offer its own Claude.ai sign-in flow.

The [Desktop documentation](https://code.claude.com/docs/en/desktop#coming-from-the-cli) says CLI and Desktop maintain separate session lists. Desktop `/resume` can import a CLI conversation. CLI `/desktop` is a handoff that exits the terminal session and requires subscription authentication; API-key and cloud-provider sessions cannot use that command. Automatic sidebar visibility or simultaneous live observation is not established. Prodex must expose these as unsupported/unverified, not imply worktrees confer native-app integration.

For the first implementation, observation covers only the process Prodex launched. Arbitrary running CLI/IDE session adoption is unsupported. After a managed process is confirmed stopped, a user can attempt an explicit terminal `claude --resume <id>` from its worktree; preserving the same permission/authentication policy requires supplying the managed flags again. Opening Desktop from an API-backed worker remains unverified. Never start another writer to the same conversation while its current worker is active.

## Remaining validation

The first CLI adapter is implemented in `crates/prodex-core/src/providers/claude.rs`. It requires a nonempty API key, removes alternate subscription/cloud auth selectors from the child environment, validates resume UUIDs, and enables only file research or approved file-editing tools. Prompt delivery, process supervision, timeout, and exit verification belong to the service. The shared initial `RunSpec` has no monetary-budget field: this slice does **not** enforce a USD ceiling. Reported usage is cumulative conversation cost, not an incremental charge. The adapter does not request token deltas; it emits complete assistant text blocks and final result metadata. Tests use synthetic protocol fixtures, not recorded model runs.

- Run a bounded API-backed task, resume its persisted ID, and exercise SIGINT with actual JSON fixtures.
- Verify tools cannot write outside the worktree under the selected flags, including symlink cases.
- Confirm the approved editing profile denies shell/external actions and reports denials intelligibly.
- Verify session storage remains resumable after service restart; reconcile unknown processes conservatively.
- Validate Desktop import manually before enabling any corresponding capability.
