//! Model Context Protocol (MCP) Stdio Server for `agy-delta`.
//!
//! Exposes interactive Delta tools to Antigravity CLI (`delta_status`, `delta_share`, `delta_join`,
//! `delta_fork`, `delta_invite`, `delta_stop`, `delta_watch_start`) and manages background
//! `delta thread watch` and `delta thread mount` child processes for the duration of the session.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::{Arc, Mutex};

use delta_core::{DeltaCli, Outbox, RemoteMessage, WatchEvent};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Active background processes managed by the MCP server during the Antigravity session.
#[derive(Default)]
pub struct ProcessManager {
    watchers: HashMap<String, Child>,
    mounts: HashMap<String, Child>,
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        for (_, mut child) in self.watchers.drain() {
            let _ = child.kill();
        }
        for (_, mut child) in self.mounts.drain() {
            let _ = child.kill();
        }
    }
}

/// Run the JSON-RPC 2.0 MCP server loop over `stdin` and `stdout`.
pub fn run_mcp_stdio(base_dir: &Path, cli: &DeltaCli) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let manager = Arc::new(Mutex::new(ProcessManager::default()));

    for line_res in stdin.lock().lines() {
        let line = line_res?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<JsonRpcRequest>(&line) else {
            continue;
        };
        if let Some(resp) = handle_rpc_request(&req, base_dir, cli, &manager) {
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            stdout.write_all(out.as_bytes())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle_rpc_request(
    req: &JsonRpcRequest,
    base_dir: &Path,
    cli: &DeltaCli,
    manager: &Arc<Mutex<ProcessManager>>,
) -> Option<Value> {
    let id = req.id.clone()?;

    match req.method.as_str() {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "agy-delta-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        })),
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": tool_definitions()
            }
        })),
        "tools/call" => {
            let name = req.params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = req.params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let result = execute_tool(name, &args, base_dir, cli, manager);
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
        }
        "ping" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        })),
        _ => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {}", req.method)
            }
        })),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "delta_status",
            "description": "Check the current Delta CLI contract, thread binding, next sequence, and share URL for an Antigravity conversation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The Antigravity conversation ID." }
                },
                "required": ["conversationId"]
            }
        }),
        json!({
            "name": "delta_share",
            "description": "Publish and share the current Antigravity session's Delta thread with your organization, returning the live delta.dev URL.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The Antigravity conversation ID." },
                    "visibility": {
                        "type": "string",
                        "enum": ["organization", "private"],
                        "default": "organization",
                        "description": "Audience visibility for the shared thread."
                    }
                },
                "required": ["conversationId"]
            }
        }),
        json!({
            "name": "delta_join",
            "description": "Join an existing shared Delta thread (by thread ID, share ID, or delta.dev link), mounting its shared worktrees and starting a live background watcher.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The local Antigravity conversation ID to bind." },
                    "threadLocator": { "type": "string", "description": "The Delta thread ID, share ID, or https://delta.dev/join/... link." }
                },
                "required": ["conversationId", "threadLocator"]
            }
        }),
        json!({
            "name": "delta_fork",
            "description": "Fork a Delta thread and its worktrees into a new independent local thread.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "threadLocator": { "type": "string", "description": "The Delta thread ID or link to fork." }
                },
                "required": ["threadLocator"]
            }
        }),
        json!({
            "name": "delta_invite",
            "description": "Invite teammates by email to collaborate on the current Delta thread.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The Antigravity conversation ID." },
                    "emails": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Email addresses to invite."
                    }
                },
                "required": ["conversationId", "emails"]
            }
        }),
        json!({
            "name": "delta_stop",
            "description": "Stop the active turn on a Delta thread immediately.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "threadLocator": { "type": "string", "description": "The Delta thread ID or link." }
                },
                "required": ["threadLocator"]
            }
        }),
        json!({
            "name": "delta_watch_start",
            "description": "Start or ensure a live background `delta thread watch --participate Antigravity` stream for the given conversation so remote comments and turn handoffs arrive in real time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The Antigravity conversation ID." }
                },
                "required": ["conversationId"]
            }
        }),
        json!({
            "name": "delta_sync_inbox",
            "description": "Fetch and drain any new remote messages, agent replies, or comments from collaborators on the Delta thread, plus optional recent thread messages.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "conversationId": { "type": "string", "description": "The Antigravity conversation ID." },
                    "includeRecentMessages": {
                        "type": "boolean",
                        "default": true,
                        "description": "Also fetch the latest 1-20 transcript messages from `delta thread show`."
                    }
                },
                "required": ["conversationId"]
            }
        }),
    ]
}

fn execute_tool(
    name: &str,
    args: &Value,
    base_dir: &Path,
    cli: &DeltaCli,
    manager: &Arc<Mutex<ProcessManager>>,
) -> Value {
    let res = match name {
        "delta_status" => tool_status(args, base_dir, cli),
        "delta_share" => tool_share(args, base_dir, cli, manager),
        "delta_join" => tool_join(args, base_dir, cli, manager),
        "delta_fork" => tool_fork(args, cli),
        "delta_invite" => tool_invite(args, base_dir, cli),
        "delta_stop" => tool_stop(args, cli),
        "delta_watch_start" => tool_watch_start(args, base_dir, cli, manager),
        "delta_sync_inbox" => tool_sync_inbox(args, base_dir, cli),
        _ => Err(anyhow::anyhow!("Unknown tool: {name}")),
    };

    match res {
        Ok(payload) => json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&payload).unwrap_or_default()
            }],
            "isError": false
        }),
        Err(err) => json!({
            "content": [{
                "type": "text",
                "text": format!("Delta tool error: {err}")
            }],
            "isError": true
        }),
    }
}

fn tool_status(args: &Value, base_dir: &Path, cli: &DeltaCli) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let contract = cli.contract()?;
    let outbox = Outbox::open(base_dir, &conv_id)?;
    let remote_status = if let Some(tid) = &outbox.state().thread_id {
        cli.status(tid, &conv_id).ok()
    } else {
        None
    };

    Ok(json!({
        "contract": contract,
        "localState": outbox.state(),
        "pendingOutboxCount": outbox.pending_records()?.len(),
        "remoteStatus": remote_status
    }))
}

fn tool_share(
    args: &Value,
    base_dir: &Path,
    cli: &DeltaCli,
    manager: &Arc<Mutex<ProcessManager>>,
) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let visibility =
        args.get("visibility").and_then(Value::as_str).unwrap_or("organization");

    let mut outbox = Outbox::open(base_dir, &conv_id)?;
    let thread_id = match outbox.state().thread_id.clone() {
        Some(tid) => tid,
        None => {
            let created = cli.create(&conv_id, None)?;
            outbox.state_mut().thread_id = Some(created.thread_id.clone());
            outbox.save_state()?;
            created.thread_id
        }
    };

    let _ = outbox.flush(cli, false);
    let shared = cli.share(&thread_id, visibility)?;
    if let Some(url) = shared.get("url").and_then(Value::as_str) {
        outbox.state_mut().share_url = Some(url.to_owned());
        outbox.save_state()?;
    }

    spawn_background_watcher(&conv_id, &thread_id, base_dir.to_path_buf(), cli, manager)?;

    Ok(shared)
}

fn tool_join(
    args: &Value,
    base_dir: &Path,
    cli: &DeltaCli,
    manager: &Arc<Mutex<ProcessManager>>,
) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let locator = required_str(args, "threadLocator")?;

    let status = cli.status(&locator, &conv_id)?;
    let thread_id = status
        .get("thread_id")
        .and_then(Value::as_str)
        .unwrap_or(&locator)
        .to_owned();

    let mut outbox = Outbox::open(base_dir, &conv_id)?;
    outbox.state_mut().thread_id = Some(thread_id.clone());
    if let Some(url) = status.get("url").and_then(Value::as_str) {
        outbox.state_mut().share_url = Some(url.to_owned());
    }
    if let Some(next_seq) = status
        .get("producer")
        .and_then(|p| p.get("next_sequence"))
        .and_then(Value::as_u64)
    {
        outbox.sync_next_sequence(next_seq)?;
    }
    outbox.save_state()?;

    let mount_child = cli.spawn_mount(&locator, &conv_id, true)?;
    if let Ok(mut guard) = manager.lock() {
        guard.mounts.insert(conv_id.clone(), mount_child);
    }

    spawn_background_watcher(&conv_id, &locator, base_dir.to_path_buf(), cli, manager)?;

    let recent_messages = cli.show_messages(&thread_id, "1-20").ok();

    Ok(json!({
        "joined": true,
        "threadId": thread_id,
        "status": status,
        "recentTranscript": recent_messages
    }))
}

fn tool_fork(args: &Value, cli: &DeltaCli) -> anyhow::Result<Value> {
    let locator = required_str(args, "threadLocator")?;
    Ok(cli.fork(&locator)?)
}

fn tool_invite(args: &Value, base_dir: &Path, cli: &DeltaCli) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let emails: Vec<String> = args
        .get("emails")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let outbox = Outbox::open(base_dir, &conv_id)?;
    let thread_id = outbox
        .state()
        .thread_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("No Delta thread bound to session {conv_id}"))?;

    Ok(cli.invite(&thread_id, &emails)?)
}

fn tool_stop(args: &Value, cli: &DeltaCli) -> anyhow::Result<Value> {
    let locator = required_str(args, "threadLocator")?;
    Ok(cli.stop(&locator)?)
}

fn tool_watch_start(
    args: &Value,
    base_dir: &Path,
    cli: &DeltaCli,
    manager: &Arc<Mutex<ProcessManager>>,
) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let outbox = Outbox::open(base_dir, &conv_id)?;
    let thread_id = outbox
        .state()
        .thread_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("No Delta thread bound to session {conv_id}"))?;

    spawn_background_watcher(&conv_id, &thread_id, base_dir.to_path_buf(), cli, manager)?;
    Ok(json!({
        "watching": true,
        "conversationId": conv_id,
        "threadId": thread_id
    }))
}

fn tool_sync_inbox(args: &Value, base_dir: &Path, cli: &DeltaCli) -> anyhow::Result<Value> {
    let conv_id = required_str(args, "conversationId")?;
    let include_recent = args
        .get("includeRecentMessages")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let outbox = Outbox::open(base_dir, &conv_id)?;
    let inbox_messages = outbox.drain_remote_messages()?;
    let recent_transcript = if include_recent {
        if let Some(tid) = &outbox.state().thread_id {
            cli.show_messages(tid, "1-20").ok()
        } else {
            None
        }
    } else {
        None
    };

    Ok(json!({
        "newInboxMessages": inbox_messages,
        "recentTranscript": recent_transcript
    }))
}

pub fn ensure_watcher_daemon(
    conv_id: &str,
    thread_locator: &str,
    base_dir: &Path,
) -> anyhow::Result<bool> {
    let outbox = Outbox::open(base_dir, conv_id)?;
    let pid_path = outbox.dir().join("watcher.pid");
    let locator_path = outbox.dir().join("watcher.locator");

    if pid_path.exists() {
        let old_pid = std::fs::read_to_string(&pid_path).unwrap_or_default().trim().to_owned();
        let old_locator = std::fs::read_to_string(&locator_path).unwrap_or_default().trim().to_owned();
        if !old_pid.is_empty() {
            let alive = std::process::Command::new("kill")
                .args(["-0", &old_pid])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if alive && old_locator == thread_locator {
                return Ok(false);
            }
            if alive {
                let _ = std::process::Command::new("kill").arg(&old_pid).status();
            }
        }
    }

    let exe = std::env::current_exe()?;
    let child = std::process::Command::new(exe)
        .args(["watch-daemon", conv_id, thread_locator])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    std::fs::write(&pid_path, child.id().to_string())?;
    std::fs::write(&locator_path, thread_locator)?;
    Ok(true)
}

pub fn run_watch_daemon(
    session_id: &str,
    thread_locator: &str,
    base_dir: &Path,
    cli: &DeltaCli,
) -> anyhow::Result<()> {
    let mut child = cli.watch(thread_locator, Some(session_id), true, None)?;
    let Some(stdout) = child.stdout.take() else {
        return Ok(());
    };

    let reader = BufReader::new(stdout);
    let mut live_phase = false;
    let mut own_actor_id: Option<String> = None;
    let mut open_items: HashMap<String, (String, String, String)> = HashMap::new();

    for line_res in reader.lines() {
        let Ok(line) = line_res else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = WatchEvent::parse(&line) else { continue };
        let Ok(mut outbox) = Outbox::open(base_dir, session_id) else { continue };

        match event {
            WatchEvent::Ready { producer, .. } => {
                if let Some(actor) = producer
                    .as_ref()
                    .and_then(|p| p.get("actor_id"))
                    .and_then(Value::as_str)
                {
                    own_actor_id = Some(actor.to_owned());
                    outbox.state_mut().actor_id = Some(actor.to_owned());
                    let _ = outbox.save_state();
                }
            }
            WatchEvent::Live => {
                live_phase = true;
            }
            WatchEvent::Item(item) => {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if id.is_empty() {
                    continue;
                }
                let kind = item
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !matches!(
                    kind.as_str(),
                    "user_message" | "agent_message" | "cross_agent_message"
                ) {
                    continue;
                }
                let item_actor = item
                    .get("author")
                    .and_then(|a| a.get("actor_id"))
                    .and_then(Value::as_str);
                if let (Some(own), Some(theirs)) = (
                    own_actor_id.as_deref().or(outbox.state().actor_id.as_deref()),
                    item_actor,
                ) {
                    if own == theirs {
                        continue;
                    }
                }

                let author = item
                    .get("author")
                    .and_then(|a| a.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or(if kind == "user_message" {
                        "Delta Collaborator"
                    } else {
                        "Delta Agent"
                    })
                    .to_owned();
                let text = item
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let is_open = item.get("open").and_then(Value::as_bool).unwrap_or(false);

                if is_open {
                    open_items.insert(id, (author, kind, text));
                } else if live_phase && !text.trim().is_empty() {
                    open_items.remove(&id);
                    let _ = outbox.push_remote_message(&RemoteMessage {
                        id,
                        author,
                        kind,
                        text,
                        quote: None,
                    });
                }
            }
            WatchEvent::ItemAppend { id, text: chunk } => {
                if let Some((_, _, text)) = open_items.get_mut(&id) {
                    text.push_str(&chunk);
                }
            }
            WatchEvent::Turn { state, user, .. } => {
                if state == "idle" && live_phase {
                    for (id, (author, kind, text)) in open_items.drain() {
                        if !text.trim().is_empty() {
                            let _ = outbox.push_remote_message(&RemoteMessage {
                                id,
                                author,
                                kind,
                                text,
                                quote: None,
                            });
                        }
                    }
                } else if state == "running" {
                    if let Some(user_val) = user {
                        outbox.state_mut().turn_fenced = true;
                        let _ = outbox.save_state();
                        if live_phase {
                            let name = user_val
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("Teammate")
                                .to_owned();
                            let _ = outbox.push_remote_message(&RemoteMessage {
                                id: format!("turn-{name}"),
                                author: name,
                                kind: "turn_takeover".into(),
                                text: "Started a turn on this Delta thread.".into(),
                                quote: None,
                            });
                        }
                    }
                }
            }
            WatchEvent::ProducerTurn { status, .. } if status == "superseded" => {
                outbox.state_mut().turn_fenced = true;
                let _ = outbox.save_state();
            }
            WatchEvent::Comment(comment) => {
                if !live_phase {
                    continue;
                }
                let id = comment
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("comment")
                    .to_owned();
                let author = comment
                    .get("author")
                    .and_then(|a| a.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("Teammate")
                    .to_owned();
                let text = comment
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let quote = comment
                    .get("target")
                    .and_then(|t| t.get("quote").or_else(|| t.get("path")))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if !text.is_empty() {
                    let _ = outbox.push_remote_message(&RemoteMessage {
                        id,
                        author,
                        kind: "comment".into(),
                        text,
                        quote,
                    });
                }
            }
            _ => {}
        }
    }

    let _ = child.wait();
    Ok(())
}

fn spawn_background_watcher(
    conv_id: &str,
    thread_locator: &str,
    base_dir: PathBuf,
    _cli: &DeltaCli,
    _manager: &Arc<Mutex<ProcessManager>>,
) -> anyhow::Result<()> {
    let _ = ensure_watcher_daemon(conv_id, thread_locator, &base_dir)?;
    Ok(())
}

fn required_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Missing required string argument `{key}`"))
}
