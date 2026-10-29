//! Antigravity CLI Lifecycle Hook Handlers (`hooks.json`).
//!
//! All hooks receive a camelCase JSON payload on `stdin` and write a camelCase JSON response on `stdout`.

use std::path::{Path, PathBuf};

use delta_core::{DeltaCli, Outbox, Producer, ToolStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::{make_live_tool_update, read_new_steps, steps_to_updates};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum HookEvent {
    SessionStart,
    PreInvocation,
    PreToolUse,
    PostToolUse,
    PostInvocation,
    Stop,
}

/// Common fields and event-specific fields passed by Antigravity CLI on `stdin`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookInput {
    pub conversation_id: String,
    #[serde(default)]
    pub workspace_paths: Vec<PathBuf>,
    #[serde(default)]
    pub transcript_path: Option<PathBuf>,
    #[serde(default)]
    pub artifact_directory_path: Option<PathBuf>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub last_user_input: Option<String>,
    #[serde(default)]
    pub step_idx: Option<u64>,
    #[serde(default)]
    pub tool_call: Option<HookToolCall>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub invocation_num: Option<u64>,
    #[serde(default)]
    pub termination_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookToolCall {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InjectedStep {
    SystemMessage {
        #[serde(rename = "systemMessage")]
        system_message: String,
    },
    UserMessage(String),
    EphemeralMessage(String),
}

impl InjectedStep {
    pub fn system(msg: impl Into<String>) -> Self {
        Self::SystemMessage { system_message: msg.into() }
    }

    pub fn ephemeral(msg: impl Into<String>) -> Self {
        Self::EphemeralMessage(msg.into())
    }
}

/// Execute the requested lifecycle hook against `base_dir` and `cli`, returning the JSON response.
pub fn handle_hook(event: HookEvent, input: &HookInput, base_dir: &Path, cli: &DeltaCli) -> Value {
    if input.conversation_id.trim().is_empty() {
        return default_response(event);
    }

    let Ok(mut outbox) = Outbox::open(base_dir, &input.conversation_id) else {
        return default_response(event);
    };

    let cli = if let Some(ws) = input.workspace_paths.first() {
        cli.clone().with_working_dir(ws)
    } else {
        cli.clone()
    };

    match event {
        HookEvent::SessionStart => handle_session_start(input, &mut outbox, &cli),
        HookEvent::PreInvocation => handle_pre_invocation(input, &mut outbox, &cli),
        HookEvent::PreToolUse => handle_pre_tool_use(input, &mut outbox, &cli),
        HookEvent::PostToolUse => handle_post_tool_use(input, &mut outbox, &cli),
        HookEvent::PostInvocation => handle_post_invocation(input, &mut outbox, &cli),
        HookEvent::Stop => handle_stop(input, &mut outbox, &cli),
    }
}

fn handle_session_start(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) -> Value {
    let mut injected = Vec::new();

    match cli.negotiate() {
        Ok(version) => {
            outbox.state_mut().contract_version = Some(version);
            let _ = outbox.save_state();
        }
        Err(err) => {
            injected.push(InjectedStep::system(format!(
                "[Delta Plugin] CLI compatibility warning: {err}"
            )));
            return serde_json::json!({ "injectSteps": injected });
        }
    }

    if outbox.state().thread_id.is_none() {
        if let Ok(created) = cli.create(&input.conversation_id, None) {
            outbox.state_mut().thread_id = Some(created.thread_id.clone());
            let _ = outbox.save_state();
            if let Some(ws) = input.workspace_paths.first() {
                let _ = cli.attach_worktree(&created.thread_id, ws);
            }
        }
    }

    if let Some(thread_id) = outbox.state().thread_id.clone() {
        if let Ok(status) = cli.status(&thread_id, &input.conversation_id) {
            if let Some(next_seq) = status
                .get("producer")
                .and_then(|p| p.get("next_sequence"))
                .and_then(Value::as_u64)
            {
                let _ = outbox.sync_next_sequence(next_seq);
            }
            if let Some(url) = status.get("url").and_then(Value::as_str) {
                outbox.state_mut().share_url = Some(url.to_owned());
                let _ = outbox.save_state();
            }
        }
    }

    serde_json::json!({ "injectSteps": injected })
}

fn handle_pre_invocation(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) -> Value {
    ensure_thread_bound(input, outbox, cli);

    if let Some(tid) = outbox.state().thread_id.as_deref() {
        let base_dir = Outbox::default_base_dir();
        let _ = crate::mcp::ensure_watcher_daemon(&input.conversation_id, tid, &base_dir);
    }

    let _ = outbox.ensure_turn_started(&cli.producer().version);
    ingest_transcript_delta(input, outbox);
    let _ = outbox.flush(cli, true);

    let mut injected = Vec::new();
    if let Ok(remote_msgs) = outbox.drain_remote_messages() {
        if !remote_msgs.is_empty() {
            let mut block = String::from(
                "[Delta Live Thread Activity] The following messages/comments arrived on the shared Delta thread since your last turn. You MUST quote and display them at the very top of your reply (under a `### Delta Activity` heading) BEFORE answering the user:\n",
            );
            for msg in remote_msgs {
                match msg.quote {
                    Some(quote) => {
                        block.push_str(&format!(
                            "- **{}** ({} on `{}`): {}\n",
                            msg.author, msg.kind, quote, msg.text
                        ));
                    }
                    None => {
                        block.push_str(&format!(
                            "- **{}** ({}): {}\n",
                            msg.author, msg.kind, msg.text
                        ));
                    }
                }
            }
            injected.push(InjectedStep::UserMessage(block));
        }
    }

    serde_json::json!({ "injectSteps": injected })
}

fn handle_pre_tool_use(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) -> Value {
    if outbox.state().turn_fenced {
        return serde_json::json!({
            "decision": "deny",
            "reason": "Delta thread turn was taken over (fenced) by another collaborator."
        });
    }

    if let (Some(step_idx), Some(tc)) = (input.step_idx, &input.tool_call) {
        let _ = outbox.ensure_turn_started(&cli.producer().version);
        let update =
            make_live_tool_update(step_idx, &tc.name, &tc.args, ToolStatus::InProgress, None);
        let _ = outbox.enqueue(update);
    }

    serde_json::json!({ "decision": "allow" })
}

fn handle_post_tool_use(input: &HookInput, outbox: &mut Outbox, _cli: &DeltaCli) -> Value {
    if let (Some(step_idx), Some(tc)) = (input.step_idx, &input.tool_call) {
        let status =
            if input.error.is_some() { ToolStatus::Failed } else { ToolStatus::Completed };
        let update = make_live_tool_update(
            step_idx,
            &tc.name,
            &tc.args,
            status,
            input.error.as_deref(),
        );
        let _ = outbox.enqueue(update);
    }
    ingest_transcript_delta(input, outbox);

    serde_json::json!({})
}

fn handle_post_invocation(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) -> Value {
    ingest_transcript_delta(input, outbox);
    let _ = outbox.flush(cli, true);

    if outbox.state().turn_fenced {
        return serde_json::json!({
            "injectSteps": [
                InjectedStep::system("[Delta] Turn stopped because a teammate took over this thread.")
            ],
            "terminationBehavior": "terminate"
        });
    }

    serde_json::json!({ "injectSteps": [] })
}

fn handle_stop(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) -> Value {
    ingest_transcript_delta(input, outbox);
    let _ = outbox.finish_turn_if_active();
    let _ = outbox.flush(cli, true);

    if let Ok(remote_msgs) = outbox.drain_remote_messages() {
        if !remote_msgs.is_empty() {
            let summary = remote_msgs
                .into_iter()
                .map(|m| format!("{}: {}", m.author, m.text))
                .collect::<Vec<_>>()
                .join("\n");
            return serde_json::json!({
                "decision": "continue",
                "reason": format!("[Delta Live Collaboration] New message from teammate:\n{summary}")
            });
        }
    }

    serde_json::json!({ "decision": "allow" })
}

fn ensure_thread_bound(input: &HookInput, outbox: &mut Outbox, cli: &DeltaCli) {
    if outbox.state().thread_id.is_none() {
        if let Ok(created) = cli.create(&input.conversation_id, None) {
            outbox.state_mut().thread_id = Some(created.thread_id.clone());
            let _ = outbox.save_state();
            if let Some(ws) = input.workspace_paths.first() {
                let _ = cli.attach_worktree(&created.thread_id, ws);
            }
        }
    }
}

fn ingest_transcript_delta(input: &HookInput, outbox: &mut Outbox) {
    let Some(transcript_path) = &input.transcript_path else {
        return;
    };
    let last_step = outbox.state().last_transcript_step;
    let Ok(steps) = read_new_steps(transcript_path, last_step) else {
        return;
    };
    if steps.is_empty() {
        return;
    }
    let (updates, max_step) = steps_to_updates(&steps);
    for update in updates {
        let _ = outbox.enqueue(update);
    }
    if let Some(max) = max_step {
        outbox.state_mut().last_transcript_step = Some(max);
        let _ = outbox.save_state();
    }
}

fn default_response(event: HookEvent) -> Value {
    match event {
        HookEvent::SessionStart | HookEvent::PreInvocation | HookEvent::PostInvocation => {
            serde_json::json!({ "injectSteps": [] })
        }
        HookEvent::PreToolUse | HookEvent::Stop => serde_json::json!({ "decision": "allow" }),
        HookEvent::PostToolUse => serde_json::json!({}),
    }
}

pub fn default_cli() -> DeltaCli {
    DeltaCli::new(Producer::antigravity(env!("CARGO_PKG_VERSION")))
}
