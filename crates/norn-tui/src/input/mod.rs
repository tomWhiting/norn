//! One plain Iridium composer, host completion and independent submission recall.

pub(crate) mod composer_clipboard;
pub mod composer_kernel;
pub(crate) mod composer_keys;
pub mod composer_transactions;
pub mod editor;
pub mod history;
pub mod keybindings;
pub(crate) mod view_shortcuts;

pub use autocomplete::{
    Acceptance, AutocompletePopup, AutocompleteTrigger, CandidateRow, FileCandidate,
    SlashCandidate, SourceTag, TriggerKind, detect_trigger, filter_slash_candidates,
    generate_file_candidates,
};
pub use editor::InputEditor;
pub use history::InputHistory;
pub use keybindings::{InputAction, map_key_event};

pub mod autocomplete;

pub use composer_kernel::ComposerError;
pub use composer_transactions::{CompletionContext, ComposerSnapshot, PreparedComposerCut};
