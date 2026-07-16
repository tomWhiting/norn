//! Public Responses item kinds retained losslessly without core projections.

use serde_json::Value;

/// A pinned public output-item kind without a dedicated core representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnownResponseItemKind {
    /// Provider-hosted file search.
    FileSearchCall,
    /// Result supplied for a function call.
    FunctionCallOutput,
    /// Computer-use request.
    ComputerCall,
    /// Computer-use result.
    ComputerCallOutput,
    /// Provider program execution request.
    Program,
    /// Provider program execution result.
    ProgramOutput,
    /// Dynamic tool search request.
    ToolSearchCall,
    /// Dynamic tool search result.
    ToolSearchOutput,
    /// Dynamically exposed tool definitions.
    AdditionalTools,
    /// Provider-hosted image generation.
    ImageGenerationCall,
    /// Provider-hosted code interpreter execution.
    CodeInterpreterCall,
    /// Local shell execution request.
    LocalShellCall,
    /// Local shell execution result.
    LocalShellCallOutput,
    /// Shell execution request.
    ShellCall,
    /// Shell execution result.
    ShellCallOutput,
    /// Patch application request.
    ApplyPatchCall,
    /// Patch application result.
    ApplyPatchCallOutput,
    /// Provider-hosted MCP invocation.
    McpCall,
    /// Provider-hosted MCP tool listing.
    McpListTools,
    /// MCP approval request requiring a client decision.
    McpApprovalRequest,
    /// MCP approval decision.
    McpApprovalResponse,
    /// Result supplied for a custom tool call.
    CustomToolCallOutput,
}

impl KnownResponseItemKind {
    /// Classify one exact public discriminator.
    #[must_use]
    pub fn from_discriminator(value: &str) -> Option<Self> {
        match value {
            "file_search_call" => Some(Self::FileSearchCall),
            "function_call_output" => Some(Self::FunctionCallOutput),
            "computer_call" => Some(Self::ComputerCall),
            "computer_call_output" => Some(Self::ComputerCallOutput),
            "program" => Some(Self::Program),
            "program_output" => Some(Self::ProgramOutput),
            "tool_search_call" => Some(Self::ToolSearchCall),
            "tool_search_output" => Some(Self::ToolSearchOutput),
            "additional_tools" => Some(Self::AdditionalTools),
            "image_generation_call" => Some(Self::ImageGenerationCall),
            "code_interpreter_call" => Some(Self::CodeInterpreterCall),
            "local_shell_call" => Some(Self::LocalShellCall),
            "local_shell_call_output" => Some(Self::LocalShellCallOutput),
            "shell_call" => Some(Self::ShellCall),
            "shell_call_output" => Some(Self::ShellCallOutput),
            "apply_patch_call" => Some(Self::ApplyPatchCall),
            "apply_patch_call_output" => Some(Self::ApplyPatchCallOutput),
            "mcp_call" => Some(Self::McpCall),
            "mcp_list_tools" => Some(Self::McpListTools),
            "mcp_approval_request" => Some(Self::McpApprovalRequest),
            "mcp_approval_response" => Some(Self::McpApprovalResponse),
            "custom_tool_call_output" => Some(Self::CustomToolCallOutput),
            _ => None,
        }
    }

    /// Return the exact public discriminator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileSearchCall => "file_search_call",
            Self::FunctionCallOutput => "function_call_output",
            Self::ComputerCall => "computer_call",
            Self::ComputerCallOutput => "computer_call_output",
            Self::Program => "program",
            Self::ProgramOutput => "program_output",
            Self::ToolSearchCall => "tool_search_call",
            Self::ToolSearchOutput => "tool_search_output",
            Self::AdditionalTools => "additional_tools",
            Self::ImageGenerationCall => "image_generation_call",
            Self::CodeInterpreterCall => "code_interpreter_call",
            Self::LocalShellCall => "local_shell_call",
            Self::LocalShellCallOutput => "local_shell_call_output",
            Self::ShellCall => "shell_call",
            Self::ShellCallOutput => "shell_call_output",
            Self::ApplyPatchCall => "apply_patch_call",
            Self::ApplyPatchCallOutput => "apply_patch_call_output",
            Self::McpCall => "mcp_call",
            Self::McpListTools => "mcp_list_tools",
            Self::McpApprovalRequest => "mcp_approval_request",
            Self::McpApprovalResponse => "mcp_approval_response",
            Self::CustomToolCallOutput => "custom_tool_call_output",
        }
    }
}

/// A pinned public item retained as exact provider JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct KnownResponseItem {
    pub(super) raw: Value,
    pub(super) kind: KnownResponseItemKind,
    pub(super) id: Option<String>,
}

impl KnownResponseItem {
    /// Return the pinned public kind.
    #[must_use]
    pub const fn kind(&self) -> KnownResponseItemKind {
        self.kind
    }
}
