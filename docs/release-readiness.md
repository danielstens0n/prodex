# Release readiness

Prodex is a working local prototype, not yet a validated public release. The Rust planning/scheduling loop exists. It observes independent Codex terminal sessions in explicitly selected projects, proposes coding work, runs approved Codex/Claude workers, and now manages background local integration. Publishing PRs is outside the current scope.

## 1. Prove the real workflow

- [ ] On a disposable project, run observed session → useful suggestion → approval → actual coding → Merge locally → independently verified completion → next suggestion, with authenticated Codex and Claude.
- [ ] Exercise missing Git, existing dirty main folders, divergent branches, overlap/conflict, failed tests, permission denial, provider timeouts, stop and retry. Never report an unmerged or uncommitted worktree as done.
- [ ] Test interruption/restart during preparation, execution, commit, merge and verification. Provide clear in-app recovery rather than requiring database edits or a CLI acknowledgement.
- [ ] Verify the installed provider versions and actual account/permission requirements. Claude currently uses API-key authentication; initial activation still observes Codex terminal sessions, not Claude/IDE-only activity.

Automated Git fixtures, mock service tests and command-generation tests cover substantial mechanics, but do not replace authenticated end-to-end acceptance. A real Ghostty command handoff has been smoke-tested; other app/provider combinations still need validation.

## 2. Make installation and operation boring

- [ ] Package the daemon with the desktop app, manage its startup/shutdown/version upgrades, and test upgrades with active and pending work without losing state.
- [ ] Ship a documented macOS installer; validate signing/notarization and fresh-install Gatekeeper/Automation behavior. Clearly state supported platforms.
- [ ] Add first-run provider detection/authentication diagnostics, a small diagnostic export, and actionable recovery messages. Do not auto-resume an intentionally paused installation when the desktop opens.
- [ ] Finish bounded event-history pagination and test long-running histories. A task implementation currently exists in a worktree; its presence does not mean it is integrated in main.
- [ ] Define storage retention, worktree cleanup after verified completion, database backup/migration policy and disk-usage limits.

## 3. Improve usefulness and control

- [ ] Dogfood across several projects and measure accepted/completed suggestions, duplicate/rejected suggestions, completion cost and interruptions.
- [ ] Let users inspect/edit each project's goal and planner notes, and record a rejection reason. Validate that planning prioritizes the user's product goal over generic cleanup.
- [ ] Make capacity/cooldown/daily limits and actual spend understandable. Monetary caps are not implemented. Keep proposal approval explicit until risk-based automation is validated.
- [ ] Decide and document whether Claude/IDE/desktop-only sessions should trigger planning; do not advertise universal observation until those adapters exist.

## 4. Prepare the public repository

- [ ] Add an actual LICENSE file consistent with the declared MIT package metadata, CONTRIBUTING guidance, a security-reporting policy and support/issue templates.
- [ ] Add CI for Rust formatting/Clippy/tests, frontend build/UI tests and native macOS builds; establish release/version/changelog automation and dependency/license review.
- [ ] Rewrite the README around installation, a short demonstration, supported workflows, privacy/data flow, permissions, costs and known limitations.
- [ ] Audit tracked files and Git history before publication. `plan.md` is ignored and removed from the current index, but remains in earlier history unless history is deliberately rewritten before publishing.

## Current verification boundary

The background-merge implementation checks Git state independently of the agent's final text, preserves original results on failure and shares task recovery/concurrency controls. It does not lock out independent editors/agents, independently certify test execution, guarantee provider permission approval, or provide a production installer. Finish the real workflow and recovery gates before adding more UI or more providers.
