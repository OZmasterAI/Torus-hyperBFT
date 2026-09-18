//! Opt-in local stages of the ordinary body request/response send path.
//! This metadata is never part of a consensus or libp2p wire message.

use std::sync::OnceLock;

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::hotstuff::messages::HotStuffMessage;
use hotstuff_rs::logging::{BodyFetchTraceId, BodyFetchTraceStamp as Stamp};
use hotstuff_rs::networking::messages::{Message, ProgressMessage};
use libp2p::request_response::OutboundRequestId;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::swarm::NetworkCommand;

fn flag(value: Option<&str>) -> bool {
    value == Some("1")
}

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| flag(std::env::var("TORUS_BODY_SEND_TRACE").ok().as_deref()))
}

#[derive(Clone, Copy)]
struct Metadata {
    kind: &'static str,
    target: [u8; 32],
    hash: [u8; 32],
    view: u64,
}

impl Metadata {
    fn from_message(target: VerifyingKey, message: &Message) -> Option<Self> {
        let Message::ProgressMessage(ProgressMessage::HotStuffMessage(message)) = message else {
            return None;
        };
        let (kind, hash, view) = match message {
            HotStuffMessage::BlockDataRequest(request) => {
                ("request", request.block_hash, request.view)
            }
            HotStuffMessage::BlockDataResponse(response) => {
                ("response", response.block.hash, response.view)
            }
            _ => return None,
        };
        Some(Self {
            kind,
            target: target.to_bytes(),
            hash: hash.bytes(),
            view: view.int(),
        })
    }

    fn json(self) -> Value {
        json!({
            "kind": self.kind,
            "target": BodyFetchTraceId(self.target).to_string(),
            "hash": BodyFetchTraceId(self.hash).to_string(),
            "view": self.view,
        })
    }
}

fn stamp(stamp: Option<Stamp>) -> Value {
    stamp.map_or(Value::Null, |s| {
        json!({
            "pid": s.pid, "seq": s.seq, "mono_us": s.mono_us, "unix_us": s.unix_us,
        })
    })
}

/// Only carried by the enabled local command variant. Boxing this context keeps
/// the normal command's enum storage from growing with every diagnostic stamp.
#[doc(hidden)]
pub struct BodySendTrace {
    meta: Metadata,
    pre_enqueue: Stamp,
    dequeue: Option<Stamp>,
    queue_remaining: Option<usize>,
    pub(crate) encode_begin: Option<Stamp>,
    pub(crate) encode_end: Option<Stamp>,
    pub(crate) send_begin: Option<Stamp>,
    pub(crate) send_end: Option<Stamp>,
    pub(crate) tracking_begin: Option<Stamp>,
    pub(crate) tracking_end: Option<Stamp>,
    pub(crate) payload_bytes: Option<usize>,
    pub(crate) request_id: Option<OutboundRequestId>,
    outcome: Option<&'static str>,
    finished: Option<Stamp>,
}

impl BodySendTrace {
    pub(crate) fn new(target: VerifyingKey, message: &Message) -> Option<Self> {
        let meta = Metadata::from_message(target, message)?;
        Some(Self {
            meta,
            pre_enqueue: Stamp::capture(),
            dequeue: None,
            queue_remaining: None,
            encode_begin: None,
            encode_end: None,
            send_begin: None,
            send_end: None,
            tracking_begin: None,
            tracking_end: None,
            payload_bytes: None,
            request_id: None,
            outcome: None,
            finished: None,
        })
    }

    pub(crate) fn dequeued(&mut self, queue_remaining: usize) {
        self.dequeue = Some(Stamp::capture());
        self.queue_remaining = Some(queue_remaining);
    }

    pub(crate) fn finish(&mut self, outcome: &'static str) {
        self.finished = Some(Stamp::capture());
        self.outcome = Some(outcome);
    }

    fn json(&self) -> Value {
        json!({
            "schema": 1, "event": "send", "pid": self.pre_enqueue.pid,
            "id": self.pre_enqueue.seq, "message": self.meta.json(),
            "pre_enqueue": stamp(Some(self.pre_enqueue)), "dequeue": stamp(self.dequeue),
            "queue_remaining": self.queue_remaining,
            "encode_begin": stamp(self.encode_begin), "encode_end": stamp(self.encode_end),
            "send_begin": stamp(self.send_begin), "send_end": stamp(self.send_end),
            "tracking_begin": stamp(self.tracking_begin),
            "tracking_end": stamp(self.tracking_end), "finished": stamp(self.finished),
            "payload_bytes": self.payload_bytes,
            "request_id": self.request_id.map(|id| format!("{id:?}")),
            "outcome": self.outcome,
        })
    }

    pub(crate) fn emit(&self) {
        // Called after the complete send path and after every map/queue lock.
        tracing::info!("body_fetch_send_diag {}", self.json());
    }

    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> Value {
        self.json()
    }
}

pub(crate) struct EnqueueRecord {
    meta: Metadata,
    pre: Stamp,
    post: Stamp,
    accepted: bool,
}

impl EnqueueRecord {
    fn emit(&self) {
        tracing::info!(
            "body_fetch_send_diag {}",
            json!({
                "schema": 1, "event": "enqueue", "pid": self.pre.pid, "id": self.pre.seq,
                "message": self.meta.json(), "pre_enqueue": stamp(Some(self.pre)),
                "post_enqueue": stamp(Some(self.post)), "accepted": self.accepted,
            })
        );
    }
}

/// Explicit enablement also permits fixtures without process-global env races.
/// No clock, JSON, box, or diagnostic record is created for OFF/non-body sends.
pub(crate) fn enqueue(
    tx: &UnboundedSender<NetworkCommand>,
    target: VerifyingKey,
    message: Message,
    enabled: bool,
) -> Option<EnqueueRecord> {
    let trace = enabled
        .then(|| BodySendTrace::new(target, &message))
        .flatten();
    let Some(trace) = trace else {
        let _ = tx.send(NetworkCommand::Send { target, message });
        return None;
    };
    let meta = trace.meta;
    let pre = trace.pre_enqueue;
    let result = tx.send(NetworkCommand::SendTraced {
        target,
        message,
        trace: Box::new(trace),
    });
    let record = EnqueueRecord {
        meta,
        pre,
        post: Stamp::capture(),
        accepted: result.is_ok(),
    };
    // Producer completion may be logged after the consumer's entire send record.
    record.emit();
    Some(record)
}

#[cfg(test)]
pub(crate) mod tests;
