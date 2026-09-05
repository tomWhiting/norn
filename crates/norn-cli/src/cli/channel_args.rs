//! Explicit channel source admission and retained-quota command-line arguments.

use std::num::NonZeroUsize;
use std::str::FromStr;

use clap::{Args, ValueEnum};
use norn::config::{ChannelOverflowSetting, ChannelPolicySetting};

/// Channel overrides combine with persistent settings before startup validation.
#[derive(Args, Debug)]
pub struct ChannelArgs {
    /// Override one configured source: NAME=off|next-turn|wake.
    ///
    /// Next-turn requires the interactive TUI. Wake in print/driven mode joins
    /// the active run only; it does not keep the process waiting after completion.
    /// Hold is unavailable until CLI inbox release/deny controls exist.
    #[arg(long = "channel", value_name = "NAME=POLICY")]
    pub channel: Vec<ChannelSourceArg>,

    /// Positive total message quota across staged, held, queued and claimed input.
    #[arg(long, value_name = "COUNT")]
    pub channel_max_retained_messages: Option<NonZeroUsize>,

    /// Positive total UTF-8 byte quota for retained source labels, content and metadata.
    #[arg(long, value_name = "BYTES")]
    pub channel_max_retained_bytes: Option<NonZeroUsize>,

    /// Explicit behavior when the retained inbox is full.
    #[arg(long, value_enum)]
    pub channel_overflow: Option<ChannelOverflowArg>,
}

/// One operator-named source with an explicit delivery policy.
#[derive(Clone, Debug)]
pub struct ChannelSourceArg {
    /// Exact configured MCP server name, independent of sender metadata.
    pub name: String,
    /// The operator-selected behavior for admitted input.
    pub policy: ChannelPolicySetting,
}

impl FromStr for ChannelSourceArg {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (name, value) = raw.split_once('=').ok_or_else(|| {
            format!("channel '{raw}' must name NAME=off, NAME=next-turn or NAME=wake")
        })?;
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(format!(
                "channel source '{name}' must be a nonempty exact name"
            ));
        }
        let policy = match value {
            "hold" => {
                return Err(format!(
                    "channel source '{name}' uses policy 'hold', unsupported in every CLI mode because inbox release/deny controls are not available"
                ));
            }
            "off" => ChannelPolicySetting::Off,
            "next-turn" => ChannelPolicySetting::NextTurn,
            "wake" => ChannelPolicySetting::Wake,
            _ => {
                return Err(format!(
                    "channel source '{name}' has unknown policy '{value}'; use off, next-turn or wake"
                ));
            }
        };
        Ok(Self {
            name: name.to_owned(),
            policy,
        })
    }
}

/// Explicit overflow strategies accepted by the CLI.
#[derive(Clone, Copy, Debug, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ChannelOverflowArg {
    /// Refuse new input visibly while continuing MCP tool responses.
    RejectNew,
}

impl From<ChannelOverflowArg> for ChannelOverflowSetting {
    fn from(value: ChannelOverflowArg) -> Self {
        match value {
            ChannelOverflowArg::RejectNew => Self::RejectNew,
        }
    }
}
