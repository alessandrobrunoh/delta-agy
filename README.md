# Delta for Antigravity CLI (`delta-agy`)

Official-style [Delta](https://delta.dev) plugin for **Google Antigravity CLI (`agy`)**, inspired by [`zed-industries/delta-claude`](https://github.com/zed-industries/delta-claude).

When active, your local terminal-based Antigravity CLI sessions — along with user prompts, model thoughts, tool calls, and Git working tree states — are automatically recorded and synced to Delta in real time. Teammates and other agents can view, comment on, and collaborate on the thread from [`delta.dev`](https://delta.dev), the Delta desktop app, or their own coding agent sessions.

---

## Architecture

`delta-agy` is built as a unified Rust workspace (`delta-core` + `agy-delta`) packaged as a native Antigravity plugin (`.agents/plugins/delta`):

- **`crates/delta-core`**: Shared ACP Schema-1 record builders (`Envelope`, `Update`, `ToolCallUpdate`), durable local outbox & remote inbox (`~/.gemini/antigravity-cli/delta-plugin/sessions/<id>/`), and `delta --json` CLI bridge (supporting contract versions `v6` through `v8`).
- **`crates/agy-delta`**:
  - **Lifecycle Hooks (`agy-delta hook <Event>`)**: Handles `SessionStart`, `PreInvocation`, `PreToolUse`, `PostToolUse`, `PostInvocation`, and `Stop` events to stream turns, enforce `fenced`/`stopped` turn ownership, and inject live teammate comments and remote agent messages into `agy`.
  - **Background Watch Daemon (`agy-delta watch-daemon`)**: Spawns and manages a persistent `delta --json thread watch` stream per conversation, capturing remote comments (`thread.comment`) and live turns (`item` / `item.append`) into the session's durable inbox.
  - **MCP Server (`agy-delta mcp`)**: JSON-RPC 2.0 stdio server exposing `delta_status`, `delta_share`, `delta_join`, `delta_fork`, `delta_invite`, `delta_stop`, `delta_watch_start`, and `delta_sync_inbox`.
  - **Direct CLI (`agy-delta <subcommand>`)**: Fast CLI fallback commands (`status`, `share`, `join`, `sync`, `contract`).

---

## Requirements

1. **Delta CLI (`delta`)**: Version `0.23.0+` (contract `v6`–`v8`) installed and authenticated (`delta auth login`).
2. **Antigravity CLI (`agy`)**: Installed on macOS / Linux.
3. **Rust Toolchain**: `cargo` (Rust 1.85+) to build the `agy-delta` binary.

---

## Installation & Build

Clone the repository and build the release binary into the Antigravity plugin bundle:

```bash
git clone https://github.com/alessandrobrunoh/delta-agy.git
cd delta-agy

cargo build --release
mkdir -p .agents/plugins/delta/bin
rm -f .agents/plugins/delta/bin/agy-delta
cp target/release/agy-delta .agents/plugins/delta/bin/agy-delta
codesign --force --sign - .agents/plugins/delta/bin/agy-delta
```

To install the plugin globally across all Antigravity CLI workspaces:

```bash
mkdir -p ~/.gemini/antigravity-cli/plugins
cp -R .agents/plugins/delta ~/.gemini/antigravity-cli/plugins/delta
```

---

## Usage in Antigravity CLI (`agy`)

Inside an `agy` session, use the `/delta:delta` skill or ask the agent directly:

- **Share current session**:
  ```text
  /delta:delta share public
  ```
- **Join an existing Delta thread**:
  ```text
  /delta:delta join https://delta.dev/join/thread_...#l=...
  ```
- **Check thread status**:
  ```text
  /delta:delta status
  ```
- **Pull latest remote messages & comments**:
  ```text
  /delta:delta sync
  ```
- **Invite a teammate**:
  ```text
  /delta:delta invite <username> write
  ```

---

## Repository Structure

```text
.
├── Cargo.toml
├── crates/
│   ├── delta-core/          # ACP Schema-1 types, Outbox/Inbox state, Delta CLI wrapper
│   └── agy-delta/           # Antigravity hooks, MCP server, watcher daemon, and CLI
└── .agents/
    └── plugins/
        └── delta/           # Antigravity plugin bundle
            ├── plugin.json
            ├── hooks.json
            ├── mcp_config.json
            ├── rules/AGENTS.md
            └── skills/delta/SKILL.md
```

## License

MIT
