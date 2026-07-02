# Norn-Tools/Apply-Patch — User Stories

## AI Agent — Applying Code Changes

**S1.** As an AI agent applying a patch, I want the tool to resolve hunks using structural understanding of the code so that my patches apply correctly even when my line numbers are stale.

**S2.** As an AI agent creating a new file, I want to generate a standard unified diff with --- /dev/null so that file creation works through the same tool as file modification.

**S3.** As an AI agent removing a file, I want to generate a unified diff with +++ /dev/null so that file deletion works through the same tool and gets proper undo tracking.

**S4.** As an AI agent, I want the tool result to tell me which resolution tier was used for each hunk so that I can calibrate my patch accuracy over time.

**S5.** As an AI agent, I want to know the exact committed state of files even when post-validation fails so that I can decide whether to fix forward or undo.

## AI Agent — Verifying Patch Accuracy

**S6.** As an AI agent verifying my own accuracy, I want to apply a patch in strict mode so that I can confirm my line numbers are correct before committing.

**S7.** As an AI agent whose strict-mode patch failed, I want to see what structural matching would have done so that I can either accept the structural match or regenerate with correct line numbers.

**S8.** As an AI agent, I want to dry-run a patch before applying so that I can preview the blast radius and diagnostics without modifying any files.

**S9.** As an AI agent, I want dry-run to use the same resolution code path as actual application so that a successful dry-run guarantees successful application.

## AI Agent — Coordinating Fork Results

**S10.** As a parent agent receiving patches from forks, I want to assign an artifact ID to a patch so that I can inspect and manipulate it without re-generating the full patch text.

**S11.** As a parent agent, I want to inspect individual files or hunks within a stored patch artifact so that I can review fork output selectively.

**S12.** As a parent agent, I want to edit a specific hunk in a stored artifact before applying so that I can fix fork errors without regenerating the entire patch.

**S13.** As a parent agent, I want to dry-run a stored artifact so that I can preview the result before committing fork changes to disk.

## Human Developer — Reviewing Agent Patches

**S14.** As a developer reviewing agent output, I want to see per-hunk resolution details so that I can understand how each change was placed and whether structural matching was needed.

**S15.** As a developer, I want the tool to support 18+ languages for entity resolution so that patches to Go, Java, C, and other codebases apply with the same structural accuracy as Rust.

**S16.** As a developer, I want dry-run output to include file length impact so that I can catch patches that would push files over the 500-line limit before they commit.

## Tool Author — Integrating Entity Extraction

**S17.** As a tool author, I want entity extraction behind an optional cargo feature flag so that I can use norn without pulling in libyggd's full dependency tree.

**S18.** As a tool author, I want the EntityExtractor trait to be testable with mock implementations so that I can write tests without needing real AST parsing infrastructure.
