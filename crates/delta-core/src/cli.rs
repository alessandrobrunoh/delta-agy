//! The `delta` CLI as an agent integration speaks it: `delta --json thread <command>`.
//!
//! The recording contract is internal to Delta and versioned; [`DeltaCli::negotiate`] refuses a CLI
//! whose range does not overlap [`CONTRACT_MIN`]..=[`CONTRACT_MAX`].

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The oldest recording contract this core speaks.
pub const CONTRACT_MIN: u32 = 6;
/// The newest recording contract this core speaks (updated for Delta 0.23.0 contract v8).
pub const CONTRACT_MAX: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum DeltaError {
    #[error("could not run delta: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("delta {command} failed ({code}/{reason}): {message}")]
    Command { command: String, code: String, reason: String, message: String, retryable: bool },
    #[error("delta {command} printed unexpected output: {detail}")]
    Protocol { command: String, detail: String },
    #[error("delta speaks recording contracts {min}..={max}; this integration needs {CONTRACT_MIN}..={CONTRACT_MAX}")]
    Incompatible { min: u32, max: u32 },
}

impl DeltaError {
    /// Whether retrying the same command later may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Spawn(_) => true,
            Self::Command { retryable, .. } => *retryable,
            Self::Protocol { .. } | Self::Incompatible { .. } => false,
        }
    }
}

/// Who records into a thread: `--source`, and the name Delta shows for its output.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Producer {
    /// The `--source` identity, such as `antigravity`.
    pub source: String,
    /// The `--producer-name` / `--participate` display name, such as `Antigravity`.
    pub name: String,
    /// The integration's own version, recorded on each turn start.
    pub version: String,
}

impl Producer {
    pub fn antigravity(version: impl Into<String>) -> Self {
        Self {
            source: "antigravity".into(),
            name: "Antigravity".into(),
            version: version.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateReceipt {
    pub thread_id: String,
    /// False when an existing thread bound to this producer was reused.
    pub created: bool,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Recorded,
    /// Refused because another participant superseded the turn it names.
    Fenced,
    Duplicate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendReceipt {
    pub thread_id: String,
    pub sequence: u64,
    pub next_sequence: u64,
    pub duplicate: bool,
    pub disposition: Disposition,
    pub turn_sequence: Option<u64>,
}

/// One line of `delta thread watch`. Unknown types stay visible as [`WatchEvent::Other`].
#[derive(Clone, Debug, PartialEq)]
pub enum WatchEvent {
    Ready { thread_id: String, producer: Option<Value> },
    Live,
    Item(Value),
    ItemAppend { id: String, text: String },
    Remove { id: String },
    /// Someone's turn started or ended; `user` is null for a producer's own turn.
    Turn { state: String, turn_id: Option<String>, user: Option<Value> },
    ProducerTurn { sequence: u64, status: String },
    Presence(Vec<Value>),
    Comment(Value),
    CommentRemove { id: String },
    Error(Value),
    Other(Value),
}

impl WatchEvent {
    pub fn parse(line: &str) -> Result<Self, serde_json::Error> {
        let value: Value = serde_json::from_str(line)?;
        let str_field = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        let event = match value.get("type").and_then(Value::as_str) {
            Some("ready") => match str_field("thread_id") {
                Some(thread_id) => Self::Ready {
                    thread_id,
                    producer: value.get("producer").cloned().filter(|p| !p.is_null()),
                },
                None => Self::Other(value),
            },
            Some("live") => Self::Live,
            Some("item") => Self::Item(value["item"].clone()),
            Some("item.append") => match (str_field("id"), str_field("text")) {
                (Some(id), Some(text)) => Self::ItemAppend { id, text },
                _ => Self::Other(value),
            },
            Some("remove") => match str_field("id") {
                Some(id) => Self::Remove { id },
                None => Self::Other(value),
            },
            Some("turn") => match str_field("state") {
                Some(state) => Self::Turn {
                    state,
                    turn_id: str_field("turn_id"),
                    user: value.get("user").cloned().filter(|u| !u.is_null()),
                },
                None => Self::Other(value),
            },
            Some("producer.turn") => match (
                value.get("sequence").and_then(Value::as_u64),
                str_field("status"),
            ) {
                (Some(sequence), Some(status)) => Self::ProducerTurn { sequence, status },
                _ => Self::Other(value),
            },
            Some("presence") => match value.get("users").and_then(Value::as_array) {
                Some(users) => Self::Presence(users.clone()),
                None => Self::Other(value),
            },
            Some("comment") => Self::Comment(value["comment"].clone()),
            Some("comment.remove") => match str_field("id") {
                Some(id) => Self::CommentRemove { id },
                None => Self::Other(value),
            },
            Some("error") => Self::Error(value["error"].clone()),
            _ => Self::Other(value),
        };
        Ok(event)
    }
}

/// Discover `delta` in `$DELTA_BIN`, `PATH`, or standard macOS/Linux app locations.
pub fn discover_delta_binary() -> PathBuf {
    if let Some(custom) = std::env::var_os("DELTA_BIN") {
        return PathBuf::from(custom);
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let candidates = [
            home.join(".local/bin/delta"),
            home.join("Applications/Delta.app/Contents/MacOS/delta"),
            PathBuf::from("/Applications/Delta.app/Contents/MacOS/delta"),
            home.join(".local/delta.app/bin/delta"),
        ];
        for candidate in candidates {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("delta")
}

/// The `delta` executable, speaking for one [`Producer`].
#[derive(Clone, Debug)]
pub struct DeltaCli {
    program: PathBuf,
    producer: Producer,
    working_dir: Option<PathBuf>,
}

impl DeltaCli {
    pub fn new(producer: Producer) -> Self {
        Self::with_program(discover_delta_binary(), producer)
    }

    pub fn with_program(program: impl Into<PathBuf>, producer: Producer) -> Self {
        Self { program: program.into(), producer, working_dir: None }
    }

    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    pub fn producer(&self) -> &Producer {
        &self.producer
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// `thread contract`: the recording contract range, read without contacting the app.
    pub fn contract(&self) -> Result<Contract, DeltaError> {
        let value = self.run_json("thread contract", &["thread", "contract"], None)?;
        parse_as("thread contract", value)
    }

    /// The newest contract both sides speak.
    pub fn negotiate(&self) -> Result<u32, DeltaError> {
        let Contract { min, max } = self.contract()?;
        if max < CONTRACT_MIN || min > CONTRACT_MAX {
            return Err(DeltaError::Incompatible { min, max });
        }
        Ok(max.min(CONTRACT_MAX))
    }

    /// `thread create`: create the thread `external_id` records into, or reuse the one bound to it.
    /// `organization` is `(org_id, "private" | "organization")`.
    pub fn create(
        &self,
        external_id: &str,
        organization: Option<(&str, &str)>,
    ) -> Result<CreateReceipt, DeltaError> {
        let mut args = self.producer_args("create", None, external_id);
        if let Some((id, visibility)) = organization {
            args.extend(["--organization", id, "--visibility", visibility].map(OsString::from));
        }
        let value = self.run_json("thread create", &args, None)?;
        parse_as("thread create", value)
    }

    /// `thread create --parent`: create or reuse a subagent child thread under `parent_thread`.
    pub fn create_child(
        &self,
        parent_thread: &str,
        external_id: &str,
        name: &str,
        shared_worktrees: bool,
    ) -> Result<CreateReceipt, DeltaError> {
        let mut args = self.producer_args("create", None, external_id);
        args.extend(["--parent", parent_thread, "--name", name].map(OsString::from));
        if shared_worktrees {
            args.push(OsString::from("--shared-worktrees"));
        } else {
            args.push(OsString::from("--isolated-worktrees"));
        }
        let value = self.run_json("thread create", &args, None)?;
        parse_as("thread create", value)
    }

    /// `thread status`: what the producer needs to resume recording, including `producer.next_sequence`.
    pub fn status(&self, thread: &str, external_id: &str) -> Result<Value, DeltaError> {
        let args = self.producer_args("status", Some(thread), external_id);
        self.run_json("thread status", &args, None)
    }

    /// `thread append`: send JSONL `input` and return one receipt per acknowledged record, in order.
    /// Fewer receipts than records means only that prefix was delivered.
    pub fn append(
        &self,
        thread: &str,
        external_id: &str,
        input: &str,
        local_ack: bool,
    ) -> Result<Vec<AppendReceipt>, DeltaError> {
        let mut args = self.producer_args("append", Some(thread), external_id);
        if local_ack {
            args.extend(["--acknowledge", "local"].map(OsString::from));
        }
        args.extend([OsString::from("--producer-name"), self.producer.name.clone().into()]);
        let stdout = self.run("thread append", &args, Some(input))?;
        let mut receipts = Vec::new();
        for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
            let value: Value = serde_json::from_str(line).map_err(|error| {
                DeltaError::Protocol { command: "thread append".into(), detail: error.to_string() }
            })?;
            if let Some(error) = command_error("thread append", &value) {
                if receipts.is_empty() {
                    return Err(error);
                }
                break;
            }
            receipts.push(parse_as("thread append", value)?);
        }
        Ok(receipts)
    }

    /// `thread share`: publish the thread to its organization; returns the CLI's answer (with `url`).
    pub fn share(&self, thread: &str, visibility: &str) -> Result<Value, DeltaError> {
        self.run_json(
            "thread share",
            &["thread", "share", thread, "--visibility", visibility],
            None,
        )
    }

    /// `thread invite`: invite people by email to a thread.
    pub fn invite(&self, thread: &str, emails: &[String]) -> Result<Value, DeltaError> {
        let mut args: Vec<OsString> = vec!["thread".into(), "invite".into(), thread.into()];
        args.extend(emails.iter().map(OsString::from));
        self.run_json("thread invite", &args, None)
    }

    /// `thread fork`: fork a thread and its worktrees into a new local thread.
    pub fn fork(&self, thread: &str) -> Result<Value, DeltaError> {
        self.run_json("thread fork", &["thread", "fork", thread], None)
    }

    /// `thread stop`: end a thread's current turn without starting another.
    pub fn stop(&self, thread: &str) -> Result<Value, DeltaError> {
        self.run_json("thread stop", &["thread", "stop", thread], None)
    }

    /// `thread show`: fetch the transcript messages of a thread as JSON.
    pub fn show_messages(&self, thread: &str, range: &str) -> Result<Value, DeltaError> {
        self.run_json(
            "thread show",
            &["thread", "show", thread, "--format", "messages", "--messages", range],
            None,
        )
    }

    /// `thread show --format summarize`: fetch the summarized thread transcript as JSON.
    pub fn show_summary(&self, thread: &str) -> Result<Value, DeltaError> {
        self.run_json(
            "thread show",
            &["thread", "show", thread, "--format", "summarize"],
            None,
        )
    }

    /// `worktree attach`: attach the current checkout directory to a thread.
    pub fn attach_worktree(&self, thread: &str, directory: &Path) -> Result<Value, DeltaError> {
        let dir_str = directory.to_string_lossy();
        self.run_json(
            "worktree attach",
            &["worktree", "attach", "--thread", thread, dir_str.as_ref()],
            None,
        )
    }

    /// `thread watch`, with piped stdout of [`WatchEvent`] lines.
    pub fn watch(
        &self,
        thread: &str,
        external_id: Option<&str>,
        participate: bool,
        owner_pid: Option<u32>,
    ) -> Result<Child, DeltaError> {
        let mut args: Vec<OsString> =
            ["--json", "thread", "watch", thread].map(OsString::from).into();
        if let Some(external_id) = external_id {
            args.extend(
                ["--source", &self.producer.source, "--external-id", external_id]
                    .map(OsString::from),
            );
            if participate {
                args.extend([OsString::from("--participate"), self.producer.name.clone().into()]);
            }
        }
        if let Some(pid) = owner_pid {
            args.extend([OsString::from("--owner-pid"), pid.to_string().into()]);
        }
        let mut cmd = Command::new(&self.program);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        Ok(cmd.spawn()?)
    }

    /// `thread mount`: keep a thread's worktrees mounted while this command is connected.
    pub fn spawn_mount(
        &self,
        thread: &str,
        owner: &str,
        join: bool,
    ) -> Result<Child, DeltaError> {
        let mut args: Vec<OsString> =
            ["--json", "thread", "mount", "--owner", owner, thread].map(OsString::from).into();
        if join {
            args.push(OsString::from("--join"));
        }
        let mut cmd = Command::new(&self.program);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        Ok(cmd.spawn()?)
    }

    fn producer_args(
        &self,
        command: &str,
        thread: Option<&str>,
        external_id: &str,
    ) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["thread".into(), command.into()];
        args.extend(thread.map(OsString::from));
        args.extend(
            ["--source", &self.producer.source, "--external-id", external_id].map(OsString::from),
        );
        args
    }

    fn run_json<S: AsRef<std::ffi::OsStr>>(
        &self,
        command: &str,
        args: &[S],
        stdin: Option<&str>,
    ) -> Result<Value, DeltaError> {
        let stdout = self.run(command, args, stdin)?;
        let value: Value = serde_json::from_str(stdout.trim()).map_err(|error| {
            DeltaError::Protocol { command: command.into(), detail: error.to_string() }
        })?;
        match command_error(command, &value) {
            Some(error) => Err(error),
            None => Ok(value),
        }
    }

    fn run<S: AsRef<std::ffi::OsStr>>(
        &self,
        command: &str,
        args: &[S],
        stdin: Option<&str>,
    ) -> Result<String, DeltaError> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("--json")
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.working_dir {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn()?;
        if let Some(input) = stdin {
            child.stdin.take().expect("stdin is piped").write_all(input.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8(output.stdout).map_err(|_| DeltaError::Protocol {
            command: command.into(),
            detail: "stdout is not UTF-8".into(),
        })?;
        if stdout.trim().is_empty() && !output.status.success() {
            return Err(DeltaError::Protocol {
                command: command.into(),
                detail: format!(
                    "{} with no output: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(stdout)
    }
}

/// `{"error":{code,reason,message,retryable}}`, which the CLI prints instead of a result.
fn command_error(command: &str, value: &Value) -> Option<DeltaError> {
    let error = value.get("error")?;
    let field = |key: &str| error.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    Some(DeltaError::Command {
        command: command.into(),
        code: field("code"),
        reason: field("reason"),
        message: field("message"),
        retryable: error.get("retryable").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn parse_as<T: serde::de::DeserializeOwned>(command: &str, value: Value) -> Result<T, DeltaError> {
    serde_json::from_value(value).map_err(|error| DeltaError::Protocol {
        command: command.into(),
        detail: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_watch_events() {
        assert_eq!(WatchEvent::parse(r#"{"type":"live"}"#).unwrap(), WatchEvent::Live);
        assert_eq!(
            WatchEvent::parse(r#"{"type":"producer.turn","sequence":4,"status":"superseded"}"#)
                .unwrap(),
            WatchEvent::ProducerTurn { sequence: 4, status: "superseded".into() }
        );
        assert!(matches!(
            WatchEvent::parse(r#"{"type":"brand-new"}"#).unwrap(),
            WatchEvent::Other(_)
        ));
    }

    #[test]
    fn reads_command_errors() {
        let value = serde_json::json!({"error":{"code":"failed","reason":"no_active_database","message":"m","retryable":false}});
        let error = command_error("thread create", &value).unwrap();
        assert!(!error.is_retryable());
        assert!(error.to_string().contains("no_active_database"));
    }
}
