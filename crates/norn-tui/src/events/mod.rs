//! Session event rendering and structured output dispatch.

pub mod schema_render;

pub use schema_render::{
    DisplayToggles, render_assistant_message, render_event, render_structured, render_thinking,
    render_tool_call, render_user_message,
};
