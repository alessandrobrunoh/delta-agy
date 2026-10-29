//! Shared core for recording agent conversations into Delta threads.
//!
//! Agent adapters (`agy-delta`) translate their native events into [`Update`]s and
//! hand them to an [`Outbox`], which assigns sequences, persists them, and delivers them in order
//! through the `delta` CLI ([`DeltaCli`]).

pub mod cli;
pub mod outbox;
pub mod record;

pub use cli::{
    AppendReceipt, Contract, CreateReceipt, DeltaCli, DeltaError, Disposition, Producer, WatchEvent,
};
pub use outbox::{Outbox, RemoteMessage, SessionState};
pub use record::{ContentBlock, Envelope, ToolCallUpdate, ToolKind, ToolStatus, TurnState, Update};
