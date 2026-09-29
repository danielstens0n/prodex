Right now, if you want to build something with AI agents, you have to constantly tell them what to do. This is more effective than the current or the old way of software engineering, where you actually did the work yourself. However, since you now spend a lot of time just waiting for the AI agent to complete, you might as well try to parallelize some of that work. It's not always so easy to think of what I can actually do once this AI agent is running.

Therefore, we want to build Prodex. The main idea is that it's a proactive version of Codex and Claude Code. While you are running, one main thread will also generate recommendations of other things you can do in the meantime to reach your overarching goal. We'll infer what the overarching goal is based on the project, such as reading many files and the code itself. You can have different modes on. We'll also generate what we think are high, medium, and low risk. If you can, you can have settings where you can set that for all low-risk recommendations: just automatically do it.

For instance, if you're building a to-do app and we can see in your main session with Codex that you are focusing on the UI, then we could take time to realize: do we need to improve the backend? Then we can do that practically. The main idea is for this to work out of the gate with Codex and Claude Code, so it should work in any terminal from the get-go: just open tabs in that terminal. Or eventually, we might even build its own interface, but it should be see-live-first, I think.

## Development

An initial Rust daemon, CLI, Codex/Claude adapters, proactive proposal loop, and
Tauri Activity/Settings window are implemented. Explicitly allowed folders activate
automatic work when an independent interactive Codex terminal session is observed;
Codex and Claude remain worker providers. Real-provider end-to-end validation and
native app handoff remain release gates.

- [Run the development build](docs/development.md)
- [Implementation plan and progress](plan.md)
- [Architecture and current limitations](docs/architecture.md)
- [Desktop setup](apps/desktop/README.md)
