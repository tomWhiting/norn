# TC-001 verification evidence

Checked Rust/source commit: `29869273bc6665692e48fa66e23251b5f08d78c0` (`2986927`). This documentation follow-on captures existing successful checks; it does not rerun builds or change the checked Rust.

| Check | Result | Complete command and output |
| --- | --- | --- |
| `cargo test -p norn-tui --lib` | Exit 0; 914 passed, 0 failed, 0 ignored | [TUI library tests](proof/tui-lib.json) |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit 0 | [Strict workspace Clippy](proof/clippy.json) |
| `cargo fmt --all --check` | Exit 0; no output | [Formatting](proof/fmt.json) |

The JSON records preserve complete combined stdout/stderr as output strings, with the exact invocation, working directory, exit code, checked source SHA and an output-byte SHA-256. These checks ran on the source snapshot committed as `2986927`; the documentation-only capture is a later commit. The original outputs remain under `var/terminal-colour/logs/` in the root Norn repository.

The regression tests cover environment classification, all four retained foreground/background depths, selection/emphasis, composer RGB/indexed degradation, every palette index under ANSI16, and the composed transcript/chrome/composer frame. Brief rendering, cluster coverage, formatting and diff whitespace are checked for this documentation update.

Live terminal startup, physical appearance/input behaviour, and the reported Ghostty terminal on the 205 remain **unrun**. No terminal capability probes, installation or deployment were performed by this lane. Venue review was pending when this evidence was captured and remains required before landing.
