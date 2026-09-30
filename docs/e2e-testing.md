# End-to-end testing

Run from the repository root:

```sh
./scripts/test-e2e.sh
```

Requirements: Rust, Git, Python 3.8+, the desktop's supported Node/npm versions, desktop dependencies (`npm ci` in `apps/desktop`), and Playwright Chromium (`npx playwright install chromium`). Local Unix sockets and loopback servers must be permitted. This suite targets macOS/Linux; native desktop launch and packaging require macOS checks separately.

The command builds the actual daemon, runs black-box workflows, Rust unit/IPC tests, and the desktop build/browser tests. It never uses your normal Prodex state or edits your projects. Each black-box test owns a temporary project, Git repository, provider executables, state directory and daemon. Git author configuration is isolated; provider stand-ins do not access accounts or make network requests.

## Coverage and boundaries

| Layer | What is exercised | What is simulated |
| --- | --- | --- |
| Black-box daemon | Compiled daemon, Unix IPC, scheduling, subprocess supervision, persistence, worktrees, Git commits/merges and independent integration verification | Codex/Claude executables emit deterministic protocol events and make fixture changes |
| Rust tests | Policy, parsing, store/migrations, Git verification, observation rules, IPC and service controls | Most provider activity; live process observation is separately opt-in |
| Browser tests | Activity/settings interaction, approval/retry/merge controls, top-three reserve promotion, layout and handoff selection | Tauri commands/service responses; installed app launches |

Black-box scenarios cover approval without premature execution; both provider protocols; local merge and restart persistence; preservation of unrelated files; failed worker retry; result dismissal preserving files; blocked merge explanation and manual integration verification; pause/stop/unconnect; main-folder editing/review; dependency blocking until integration; ten ranked suggestions; duplicate/rejected proposal controls; timeout/malformed streams; and crash recovery requiring explicit acknowledgement.

These green tests prove deterministic integration behavior, not LLM quality, current provider authentication/permission compatibility, native app handoff, or signed installation. Real-provider acceptance remains a distinct release gate: use disposable projects to run observed session → suggestion → approval → coding → verified local merge → next suggestion with each provider, then stop/retry/restart/conflict scenarios. Claude's managed adapter requires `ANTHROPIC_API_KEY`; never put credentials in test fixtures or reports.

For faster iteration after building:

```sh
python3 -m unittest discover -s tests/e2e -p 'test_*.py' -v
```

Tests have bounded startup/workflow waits and print the daemon log on timeout. The slow-provider fixture is terminated by stop, timeout or explicit cleanup during the crash test. Test directories are removed afterward; the suite leaves normal Prodex state untouched.
