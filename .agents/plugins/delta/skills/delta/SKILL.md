---
name: delta
description: >-
  Manage multiplayer Delta threads, share live Antigravity CLI sessions to delta.dev,
  join or fork existing Delta threads, invite teammates, and synchronize worktrees.
  Activate when the user mentions Delta, /delta, sharing a session, joining a thread link,
  or collaborating across machines.
---

# Delta CLI Integration Skill for Antigravity

Use this skill to collaborate on live threads and worktrees with **Delta** (`delta.dev`).

## 1. Available MCP Tools (Preferred)

Always pass the current `Conversation ID` (found in `<user_information>`) as `conversationId`:

- **`delta_status`**: Inspect the negotiated CLI contract, bound `thread_id`, `next_sequence`, pending outbox records, and `share_url`.
- **`delta_share`**: Publish the current Antigravity session to your Delta organization (`visibility: "organization"` or `"private"`), start the live `delta thread watch` stream, and return the `https://delta.dev/join/...` URL.
- **`delta_join`**: Bind this conversation to an existing Delta thread ID or `https://delta.dev/join/...` URL, mount its shared worktrees (`delta thread mount --join`), and start the live background watcher.
- **`delta_fork`**: Fork an existing Delta thread and its worktrees into a new independent local thread.
- **`delta_invite`**: Invite teammates by email to the current thread.
- **`delta_watch_start`**: Ensure the background `delta thread watch --participate Antigravity` process is running for this session.
- **`delta_sync_inbox`**: Drain any new remote messages, comments, or agent replies from the Delta thread and fetch the latest transcript messages.
- **`delta_stop`**: End the current turn on a thread immediately.

## 2. Direct CLI Fallback Commands

If the MCP server is not active, you can invoke `agy-delta` or `delta` directly via `run_command`:

```bash
# Verify contract compatibility
.agents/plugins/delta/bin/agy-delta contract

# Inspect local session binding
.agents/plugins/delta/bin/agy-delta status <CONVERSATION_ID>

# Join an existing Delta thread directly
.agents/plugins/delta/bin/agy-delta join <CONVERSATION_ID> "<THREAD_ID_OR_URL>"

# Share the current session's thread
.agents/plugins/delta/bin/agy-delta share <CONVERSATION_ID> --visibility organization

# Sync and read remote inbox + recent messages
.agents/plugins/delta/bin/agy-delta sync <CONVERSATION_ID>
```
