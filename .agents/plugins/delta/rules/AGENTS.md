# Delta Multiplayer Collaboration Rules

This workspace is connected to **Delta** (`delta.dev`) through the `delta` Antigravity plugin (`agy-delta`).

## How It Works Automatically
- Every turn, prompt, thought, tool execution, and workspace change is recorded into a local durable outbox and synced via `delta --json thread append` by the plugin's lifecycle hooks (`hooks.json`).
- If a collaborator takes over the thread on Delta (`disposition: "fenced"` or remote turn start), the hooks will automatically stop or gate local tool execution and inject the teammate's messages or comments into your context.

## Interactive Delta Operations
When the user asks to **share**, **join**, **fork**, **invite teammates**, or **check status** on Delta:
1. Activate the `delta` skill (`.agents/plugins/delta/skills/delta/SKILL.md`).
2. Prefer calling the `delta` MCP server tools (`delta_status`, `delta_share`, `delta_join`, `delta_fork`, `delta_invite`, `delta_stop`, `delta_watch_start`, `delta_sync_inbox`), passing the current `Conversation ID` from `<user_information>`.
3. Whenever `[Delta Live Thread Activity]` is injected into your context (or returned by `delta_sync_inbox` / `delta_join`), **always display the remote messages/comments clearly to the user** at the top of your response so the user can see what teammates and other agents said in the Delta app.
