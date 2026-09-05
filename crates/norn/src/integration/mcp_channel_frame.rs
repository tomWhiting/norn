//! Escaped public channel frames with configured attribution and untrusted metadata.

use super::mcp_channels::McpChannelMessage;
use crate::r#loop::inbound::escape_xml;

/// Build a user-role context frame, never a trusted system or permission instruction.
///
/// Valid metadata retains its public attribute shape. A supplied `source` stays
/// visible as a separate untrusted child field so it cannot replace or duplicate
/// the host-controlled source attribute. Source instructions are handled by the
/// owning runtime, not injected as authority by this formatter.
pub fn frame_mcp_channel_message(message: &McpChannelMessage) -> String {
    let mut frame = format!("<channel source=\"{}\"", escape_xml(message.source()));
    for (key, value) in message.meta() {
        if key != "source" {
            frame.push(' ');
            frame.push_str(key);
            frame.push_str("=\"");
            frame.push_str(&escape_xml(value));
            frame.push('"');
        }
    }
    frame.push_str(">\n");
    if let Some(source) = message.meta().get("source") {
        frame.push_str("<untrusted_channel_metadata key=\"source\">");
        frame.push_str(&escape_xml(source));
        frame.push_str("</untrusted_channel_metadata>\n");
    }
    frame.push_str(&escape_xml(message.content()));
    frame.push_str("\n</channel>");
    frame
}
