use std::fs;

use super::open::new_index_entry;
use super::*;
use crate::session::branch::ROOT_PATH_ADDRESS;
use crate::session::events::{ChildBranchKind, EventBase, EventUsage, SessionEvent};
use crate::session::persistence::index::{
    append_index_entry, index_file_path, publish_new_session, read_index, update_index_entry,
};
use crate::session::persistence::io::{read_session_events, session_file_path};
use crate::session::persistence::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionRecordOrigin, SessionStatus,
};
use crate::session::store::DurabilityPolicy;
use uuid::Uuid;

fn options(model: &str) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn options_in(model: &str, working_dir: &str) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: working_dir.to_owned(),
        name: None,
    }
}

fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_owned(),
    }
}

fn assistant_with_usage(input: u64, output: u64, cache_read: u64) -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "ok".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            cost_usd: None,
        },
        stop_reason: "stop".to_owned(),
        response_id: None,
    }
}

include!("tests/lifecycle.rs");
include!("tests/open_id.rs");
include!("tests/admin.rs");
include!("tests/resume_policy.rs");
include!("tests/fork_audio.rs");
