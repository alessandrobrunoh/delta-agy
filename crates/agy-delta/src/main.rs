//! `agy-delta`: Unified Delta CLI Plugin for Antigravity CLI.

pub mod hooks;
pub mod mcp;
pub mod transcript;

use std::io::Read;

use clap::{Parser, Subcommand};
use delta_core::Outbox;
use hooks::{default_cli, handle_hook, HookEvent, HookInput};

#[derive(Parser)]
#[command(name = "agy-delta", version, about = "Delta CLI integration plugin for Antigravity CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Handle an Antigravity CLI lifecycle hook (reads JSON payload from stdin, writes JSON to stdout).
    Hook {
        #[arg(value_enum)]
        event: HookEvent,
    },
    /// Run the Model Context Protocol (MCP) server over stdio.
    Mcp,
    /// Check Delta CLI contract compatibility and print negotiated version.
    Contract,
    /// Inspect local session state and Delta thread status.
    Status {
        /// Antigravity conversation ID.
        conversation_id: String,
    },
    /// Join an existing shared Delta thread and print its status and recent messages.
    Join {
        /// Antigravity conversation ID.
        conversation_id: String,
        /// Delta thread ID, share ID, or https://delta.dev/join/... URL.
        thread_locator: String,
    },
    /// Share the current Antigravity conversation's Delta thread and print the URL.
    Share {
        /// Antigravity conversation ID.
        conversation_id: String,
        /// Audience visibility (`organization` or `private`).
        #[arg(long, default_value = "organization")]
        visibility: String,
    },
    /// Drain the local inbox of remote Delta messages/comments and fetch the latest transcript messages.
    Sync {
        /// Antigravity conversation ID.
        conversation_id: String,
    },
    /// Internal background daemon that streams `delta thread watch` into the session inbox.
    #[command(hide = true)]
    WatchDaemon {
        conversation_id: String,
        thread_locator: String,
    },
}

fn main() -> anyhow::Result<()> {
    let parsed = Cli::parse();
    let base_dir = Outbox::default_base_dir();
    let cli = default_cli();

    match parsed.command {
        Commands::Hook { event } => {
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw)?;
            let input: HookInput = if raw.trim().is_empty() {
                HookInput::default()
            } else {
                serde_json::from_str(&raw).unwrap_or_default()
            };
            let output = handle_hook(event, &input, &base_dir, &cli);
            println!("{}", serde_json::to_string(&output)?);
        }
        Commands::Mcp => {
            mcp::run_mcp_stdio(&base_dir, &cli)?;
        }
        Commands::Contract => {
            let contract = cli.contract()?;
            let negotiated = cli.negotiate()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "deltaBinary": cli.program(),
                    "min": contract.min,
                    "max": contract.max,
                    "negotiated": negotiated,
                }))?
            );
        }
        Commands::Status { conversation_id } => {
            let outbox = Outbox::open(&base_dir, &conversation_id)?;
            println!("{}", serde_json::to_string_pretty(outbox.state())?);
        }
        Commands::Join { conversation_id, thread_locator } => {
            let status = cli.status(&thread_locator, &conversation_id)?;
            let thread_id = status
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&thread_locator)
                .to_owned();
            let mut outbox = Outbox::open(&base_dir, &conversation_id)?;
            outbox.state_mut().thread_id = Some(thread_id.clone());
            if let Some(url) = status.get("url").and_then(serde_json::Value::as_str) {
                outbox.state_mut().share_url = Some(url.to_owned());
            }
            if let Some(next_seq) = status
                .get("producer")
                .and_then(|p| p.get("next_sequence"))
                .and_then(serde_json::Value::as_u64)
            {
                outbox.sync_next_sequence(next_seq)?;
            }
            outbox.save_state()?;
            let _ = mcp::ensure_watcher_daemon(&conversation_id, &thread_locator, &base_dir);
            let recent = cli.show_messages(&thread_id, "1-20").ok();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "joined": true,
                    "threadId": thread_id,
                    "watcherStarted": true,
                    "status": status,
                    "recentTranscript": recent,
                }))?
            );
        }
        Commands::Share { conversation_id, visibility } => {
            let mut outbox = Outbox::open(&base_dir, &conversation_id)?;
            let thread_id = match outbox.state().thread_id.clone() {
                Some(tid) => tid,
                None => {
                    let created = cli.create(&conversation_id, None)?;
                    outbox.state_mut().thread_id = Some(created.thread_id.clone());
                    outbox.save_state()?;
                    created.thread_id
                }
            };
            let _ = outbox.flush(&cli, false);
            let shared = cli.share(&thread_id, &visibility)?;
            if let Some(url) = shared.get("url").and_then(serde_json::Value::as_str) {
                outbox.state_mut().share_url = Some(url.to_owned());
                outbox.save_state()?;
            }
            let _ = mcp::ensure_watcher_daemon(&conversation_id, &thread_id, &base_dir);
            println!("{}", serde_json::to_string_pretty(&shared)?);
        }
        Commands::Sync { conversation_id } => {
            let outbox = Outbox::open(&base_dir, &conversation_id)?;
            if let Some(tid) = outbox.state().thread_id.as_deref() {
                let _ = mcp::ensure_watcher_daemon(&conversation_id, tid, &base_dir);
            }
            let inbox = outbox.drain_remote_messages()?;
            let recent = outbox
                .state()
                .thread_id
                .as_deref()
                .and_then(|tid| cli.show_messages(tid, "1-20").ok());
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "newInboxMessages": inbox,
                    "recentTranscript": recent,
                }))?
            );
        }
        Commands::WatchDaemon { conversation_id, thread_locator } => {
            mcp::run_watch_daemon(&conversation_id, &thread_locator, &base_dir, &cli)?;
        }
    }

    Ok(())
}
