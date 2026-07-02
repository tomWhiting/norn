//! The `initialize` handshake result (`DRIVEN-PROTOCOL.md` "Initialize and
//! capabilities").

use serde_json::{Value, json};

/// The driven-protocol contract version consumers gate on.
///
/// This names the wire contract specified in
/// `docs/design/norn-cli/DRIVEN-PROTOCOL.md` — the method set, the one-shot
/// run lifecycle, the `event/*` notification shapes, the `intervene/*`
/// primitives, and the typed stop envelope. It is distinct from the
/// `jsonrpc: "2.0"` envelope tag (which only names the JSON-RPC framing)
/// and is bumped when the contract changes incompatibly.
pub const DRIVEN_PROTOCOL_VERSION: &str = "norn-driven/1";

/// The capabilities Norn advertises in its `initialize` response.
///
/// Norn advertises the consumer-neutral intervention primitives it can
/// serve — `inject_message` and `cancel` — which the mid-run intervene loop
/// maps onto the real Norn control channel. The remaining primitives are
/// absent (unsupported), which is the honest advertisement until their
/// mechanism lands. `runLifecycle: "one_shot"` advertises that the channel
/// serves exactly one `run/execute` per process — a second one is answered
/// with the invalid-state error (`DRIVEN-PROTOCOL.md` "One-shot run
/// lifecycle").
#[must_use]
pub fn initialize_capabilities() -> Value {
    json!({
        "protocol": DRIVEN_PROTOCOL_VERSION,
        "serverInfo": {
            "name": "norn",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            // The methods this driven channel serves inbound.
            "methods": ["initialize", "run/execute"],
            // The `event/*` notification methods it emits outbound.
            "events": [
                "event/message",
                "event/toolCall",
                "event/toolResult",
                "event/progress",
                "event/stop",
                "event/raw",
            ],
            // Neutral intervention primitives Norn can serve mid-run.
            // Absent primitives (pause_resume, update_budget,
            // respond_to_approval) are unsupported until their mechanism
            // exists.
            "interventions": ["inject_message", "cancel"],
            // Exactly one run/execute is served per process.
            "runLifecycle": "one_shot",
        },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn initialize_capabilities_advertises_interventions() {
        let caps = initialize_capabilities();
        let interventions = caps["capabilities"]["interventions"].as_array().unwrap();
        let names: Vec<&str> = interventions.iter().filter_map(Value::as_str).collect();
        assert!(names.contains(&"inject_message"));
        assert!(names.contains(&"cancel"));
        // The unsupported primitives are absent — the honest advertisement.
        assert!(!names.contains(&"pause_resume"));
        assert_eq!(caps["serverInfo"]["name"], "norn");
    }

    #[test]
    fn initialize_result_carries_contract_version_and_lifecycle() {
        let caps = initialize_capabilities();
        // The contract version consumers gate on — NOT the JSON-RPC "2.0"
        // envelope tag.
        assert_eq!(caps["protocol"], json!(DRIVEN_PROTOCOL_VERSION));
        assert_eq!(caps["capabilities"]["runLifecycle"], json!("one_shot"));
    }
}
