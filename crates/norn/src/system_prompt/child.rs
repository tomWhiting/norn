//! Child base-instruction assembly for variant spawns (brief
//! `agent-variants` R3).
//!
//! A spawned child launched through a variant gets a base system
//! instruction built from the variant's prompt block followed by the
//! task — the child-path counterpart of the profile path's
//! `system_instructions`, and a drop-in replacement for the no-variant
//! literal (`"You are a sub-agent. Task: …"`), keeping the same
//! task-embedding and complete-and-stop framing so the two shapes cannot
//! drift apart.

/// Build a variant child's base system instruction: the variant's prompt
/// block first, then the task, then the standing complete-and-stop
/// instruction.
///
/// The variant prompt is trimmed of trailing whitespace (prompt files
/// conventionally end with a newline) so the composed instruction has
/// stable spacing regardless of the prompt's source (inline string,
/// prompt file, or built-in).
#[must_use]
pub fn build_child_system_prompt(variant_prompt: &str, task: &str) -> String {
    format!(
        "{}\n\nTask: {task}\n\nComplete the task and stop.",
        variant_prompt.trim_end(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The variant prompt leads, the task follows, and the standing
    /// complete-and-stop instruction closes — matching the no-variant
    /// literal's framing.
    #[test]
    fn variant_prompt_precedes_task_and_stop_instruction() {
        let built = build_child_system_prompt("You explore code.", "map the crate");
        assert_eq!(
            built,
            "You explore code.\n\nTask: map the crate\n\nComplete the task and stop.",
        );
    }

    /// Prompt-file text ends with a newline by convention; the composed
    /// instruction normalises the seam instead of emitting a blank run.
    #[test]
    fn trailing_prompt_whitespace_is_normalised() {
        let built = build_child_system_prompt("You explore code.\n", "map the crate");
        assert_eq!(
            built,
            "You explore code.\n\nTask: map the crate\n\nComplete the task and stop.",
        );
    }
}
