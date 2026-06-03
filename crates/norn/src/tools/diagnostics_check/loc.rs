//! File-length (tokei) post-check. Runs independently of LSP — CO8.

use std::path::Path;

use diagnostics::conventions::{Handling, LocCheck};

use crate::tool::lifecycle::{Advisory, AdvisorySeverity};

pub(super) fn run_loc_check(
    file_path: &Path,
    loc: &LocCheck,
    errors: &mut Vec<String>,
    advisories: &mut Vec<Advisory>,
) {
    let Some(code_lines) = diagnostics::conventions::count_code_lines(file_path) else {
        return;
    };

    if code_lines <= loc.limit {
        return;
    }

    let message = format!(
        "{}:{} [file_length] file has {code_lines} lines of code (limit: {}).\n  \
         WHY: Files over the configured LOC limit become hard to navigate and review.\n  \
         FIX: Extract related functions into a sub-module.",
        file_path.display(),
        code_lines,
        loc.limit,
    );

    match loc.handling {
        Handling::Advise => advisories.push(Advisory {
            severity: AdvisorySeverity::Warning,
            message,
            source: "file_length".to_owned(),
        }),
        Handling::Block => errors.push(message),
    }
}
