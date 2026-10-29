//! Durable local outbox and session state for Delta recording.
//!
//! Each Antigravity conversation stores its state in `<root>/<session_id>/`:
//! - `state.json`: bound `thread_id`, `next_sequence`, `current_turn_sequence`, `fenced` status,
//!   `last_transcript_step`, and `share_url`.
//! - `pending.jsonl`: unacknowledged [`Envelope`] records waiting for `delta thread append` receipt.
//! - `inbox.jsonl`: remote messages or comments received from teammates via `delta thread watch`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::{DeltaCli, DeltaError, Disposition};
use crate::record::{Envelope, TurnState, Update};

/// Maximum bytes sent in a single `delta thread append` batch (matching `delta-claude`'s 2 MB batch limit).
const MAX_BATCH_BYTES: usize = 2_000_000;

/// Persistent state for a single agent conversation session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_url: Option<String>,
    /// Next monotonically increasing sequence number to assign (starts at 1).
    pub next_sequence: u64,
    /// Sequence number of the `state_update: running` envelope that opened the active turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_turn_sequence: Option<u64>,
    /// Set to `true` when Delta returns `disposition: fenced` or a remote user supersedes our turn.
    #[serde(default)]
    pub turn_fenced: bool,
    /// Highest `step_index` already ingested from Antigravity's `transcript.jsonl`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_transcript_step: Option<u64>,
    /// Negotiated Delta CLI contract version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_version: Option<u32>,
    /// The `actor_id` Delta assigns to this producer, used to distinguish our own items from remote items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
}

impl SessionState {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            thread_id: None,
            share_url: None,
            next_sequence: 1,
            current_turn_sequence: None,
            turn_fenced: false,
            last_transcript_step: None,
            contract_version: None,
            actor_id: None,
        }
    }
}

/// A remote message or comment received from a teammate on the shared Delta thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteMessage {
    pub id: String,
    pub author: String,
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
}

/// Durable session outbox backed by the filesystem.
#[derive(Debug)]
pub struct Outbox {
    dir: PathBuf,
    state: SessionState,
}

impl Outbox {
    /// Default base directory (`~/.gemini/antigravity-cli/delta-plugin/sessions`).
    pub fn default_base_dir() -> PathBuf {
        if let Some(custom) = std::env::var_os("AGY_DELTA_STATE_DIR") {
            return PathBuf::from(custom);
        }
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        home.join(".gemini/antigravity-cli/delta-plugin/sessions")
    }

    /// Load or initialize the outbox for `session_id` inside `base_dir`.
    pub fn open(base_dir: &Path, session_id: &str) -> std::io::Result<Self> {
        let dir = base_dir.join(sanitize_session_id(session_id));
        fs::create_dir_all(&dir)?;
        let state_path = dir.join("state.json");
        let state = if state_path.exists() {
            let raw = fs::read_to_string(&state_path)?;
            serde_json::from_str(&raw).unwrap_or_else(|_| SessionState::new(session_id))
        } else {
            let initial = SessionState::new(session_id);
            write_atomic_json(&state_path, &initial)?;
            initial
        };
        Ok(Self { dir, state })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut SessionState {
        &mut self.state
    }

    pub fn save_state(&self) -> std::io::Result<()> {
        write_atomic_json(&self.dir.join("state.json"), &self.state)
    }

    /// Reconcile `next_sequence` with Delta's `thread status` if no local records are pending.
    pub fn sync_next_sequence(&mut self, remote_next_sequence: u64) -> std::io::Result<()> {
        if self.pending_records()?.is_empty() && remote_next_sequence > self.state.next_sequence {
            self.state.next_sequence = remote_next_sequence;
            self.save_state()?;
        }
        Ok(())
    }

    /// Enqueue an [`Update`], assigning the next sequence and linking it to the active turn.
    ///
    /// Rules enforced by Delta's Schema-1 contract:
    /// - `StateUpdate { state: Running }` starts a new turn and sets `turn_sequence = Some(sequence)`.
    /// - `StateUpdate { state: Idle }` closes the active turn and carries that turn's `turn_sequence`.
    /// - `SessionInfoUpdate` is not turn-scoped (`turn_sequence = None`).
    /// - All other updates inherit `current_turn_sequence`.
    pub fn enqueue(&mut self, update: Update) -> std::io::Result<Envelope> {
        let seq = self.state.next_sequence;
        self.state.next_sequence += 1;

        let turn_seq = match &update {
            Update::StateUpdate { state: TurnState::Running, .. } => {
                self.state.current_turn_sequence = Some(seq);
                self.state.turn_fenced = false;
                Some(seq)
            }
            Update::StateUpdate { state: TurnState::Idle, .. } => {
                let active = self.state.current_turn_sequence.take();
                active
            }
            Update::SessionInfoUpdate { .. } => None,
            _ => self.state.current_turn_sequence,
        };

        let envelope = Envelope::new(seq, turn_seq, update);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.pending_path())?;
        file.write_all(envelope.to_line().as_bytes())?;
        file.flush()?;
        self.save_state()?;
        Ok(envelope)
    }

    /// Start a turn (`state_update: running`) only if no turn is currently active.
    pub fn ensure_turn_started(&mut self, producer_version: &str) -> std::io::Result<u64> {
        if let Some(seq) = self.state.current_turn_sequence {
            return Ok(seq);
        }
        let env = self.enqueue(Update::running(producer_version))?;
        Ok(env.sequence)
    }

    /// End the active turn (`state_update: idle`) if one is currently active.
    pub fn finish_turn_if_active(&mut self) -> std::io::Result<Option<Envelope>> {
        if self.state.current_turn_sequence.is_none() {
            return Ok(None);
        }
        let env = self.enqueue(Update::idle())?;
        Ok(Some(env))
    }

    /// Read all unacknowledged envelopes in order.
    pub fn pending_records(&self) -> std::io::Result<Vec<Envelope>> {
        let path = self.pending_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(path)?;
        let mut records = Vec::new();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(env) = serde_json::from_str::<Envelope>(line) {
                records.push(env);
            }
        }
        Ok(records)
    }

    /// Flush pending records to `delta thread append`.
    /// Removes acknowledged prefix from `pending.jsonl` and sets `turn_fenced = true` if any
    /// receipt returns [`Disposition::Fenced`].
    pub fn flush(&mut self, cli: &DeltaCli, local_ack: bool) -> Result<usize, DeltaError> {
        let Some(thread_id) = self.state.thread_id.clone() else {
            return Ok(0);
        };
        let pending = self.pending_records()?;
        if pending.is_empty() {
            return Ok(0);
        }

        let mut total_acknowledged = 0usize;
        let mut remaining = pending.as_slice();

        while !remaining.is_empty() {
            let mut batch_input = String::new();
            let mut batch_count = 0usize;

            for env in remaining {
                let line = env.to_line();
                if batch_count > 0 && batch_input.len() + line.len() > MAX_BATCH_BYTES {
                    break;
                }
                batch_input.push_str(&line);
                batch_count += 1;
            }

            let receipts = cli.append(
                &thread_id,
                &self.state.session_id,
                &batch_input,
                local_ack,
            )?;

            let mut matched = 0usize;
            for (receipt, expected) in receipts.iter().zip(remaining.iter().take(batch_count)) {
                if receipt.sequence != expected.sequence
                    || receipt.turn_sequence != expected.turn_sequence
                {
                    break;
                }
                if receipt.disposition == Disposition::Fenced {
                    self.state.turn_fenced = true;
                }
                matched += 1;
            }

            total_acknowledged += matched;
            remaining = &remaining[matched..];

            // Persist the updated pending queue after each batch
            self.write_pending(remaining)?;
            self.save_state()?;

            if matched < batch_count {
                // Partial receipt: stop and retry the rest on the next turn
                break;
            }
        }

        Ok(total_acknowledged)
    }

    /// Append a remote message/comment received from `delta thread watch` into the inbox queue,
    /// deduplicating by `id` if an identical entry is already waiting.
    pub fn push_remote_message(&self, msg: &RemoteMessage) -> std::io::Result<()> {
        let existing = self.peek_remote_messages()?;
        if existing.iter().any(|m| m.id == msg.id && m.text == msg.text) {
            return Ok(());
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.inbox_path())?;
        let mut line = serde_json::to_string(msg).expect("RemoteMessage serializes");
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()
    }

    /// Read all queued remote messages without removing them.
    pub fn peek_remote_messages(&self) -> std::io::Result<Vec<RemoteMessage>> {
        let path = self.inbox_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let raw = fs::read_to_string(&path)?;
        let mut messages = Vec::new();
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(msg) = serde_json::from_str::<RemoteMessage>(line) {
                messages.push(msg);
            }
        }
        Ok(messages)
    }

    /// Drain all queued remote messages so they can be injected into Antigravity's context once.
    pub fn drain_remote_messages(&self) -> std::io::Result<Vec<RemoteMessage>> {
        let messages = self.peek_remote_messages()?;
        let path = self.inbox_path();
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        Ok(messages)
    }

    fn pending_path(&self) -> PathBuf {
        self.dir.join("pending.jsonl")
    }

    fn inbox_path(&self) -> PathBuf {
        self.dir.join("inbox.jsonl")
    }

    fn write_pending(&self, records: &[Envelope]) -> std::io::Result<()> {
        let path = self.pending_path();
        if records.is_empty() {
            if path.exists() {
                fs::remove_file(path)?;
            }
            return Ok(());
        }
        let mut content = String::new();
        for env in records {
            content.push_str(&env.to_line());
        }
        write_atomic_bytes(&path, content.as_bytes())
    }
}

fn sanitize_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).expect("state serializes");
    write_atomic_bytes(path, &bytes)
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assigns_sequences_and_tracks_turns() {
        let tmp = tempfile::tempdir().unwrap();
        let mut outbox = Outbox::open(tmp.path(), "sess-1").unwrap();

        let turn_seq = outbox.ensure_turn_started("0.1.0").unwrap();
        assert_eq!(turn_seq, 1);

        let user = outbox.enqueue(Update::user_text("u1", "Hello Delta")).unwrap();
        assert_eq!(user.sequence, 2);
        assert_eq!(user.turn_sequence, Some(1));

        let idle = outbox.finish_turn_if_active().unwrap().unwrap();
        assert_eq!(idle.sequence, 3);
        assert_eq!(idle.turn_sequence, Some(1));
        assert_eq!(outbox.state().current_turn_sequence, None);

        let pending = outbox.pending_records().unwrap();
        assert_eq!(pending.len(), 3);
    }

    #[test]
    fn drains_remote_inbox_once() {
        let tmp = tempfile::tempdir().unwrap();
        let outbox = Outbox::open(tmp.path(), "sess-2").unwrap();
        outbox
            .push_remote_message(&RemoteMessage {
                id: "c1".into(),
                author: "Ada".into(),
                kind: "comment".into(),
                text: "Can we use a BTreeMap here?".into(),
                quote: Some("HashMap::new()".into()),
            })
            .unwrap();

        let drained = outbox.drain_remote_messages().unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].author, "Ada");
        assert!(outbox.drain_remote_messages().unwrap().is_empty());
    }
}
