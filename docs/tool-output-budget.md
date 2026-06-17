# Tool Output Budgets

Tool results must never place unbounded text into the next model request.
This protects cost, latency, replay safety, and small/local models whose useful
context is much smaller than the largest hosted model windows.

## Policy

- `read` defaults to a bounded first window when `limit` is omitted.
- `read` is capped by both line count and character count.
- Very long physical lines are sampled and reported with warning metadata.
- Bash output is read-equivalent when it prints file contents through commands
  such as `cat`, `bat`, `sed`, `head`, or `tail`.
- Any tool result that still exceeds the generic model-facing cap is replaced
  with a bounded head/tail sample before it is persisted, streamed, recorded in
  the action log, or sent to the provider.

## Model-Aware Defaults

When a model context window is known, Norn derives the default read character
budget from that window and clamps it between conservative bounds. Large models
do not get unbounded reads; small models get smaller chunks. Explicitly large
read requests are still subject to hard caps.

## Noise Warnings

Read results can include warning metadata when content looks like low-value
context, for example generated Rust `target/.fingerprint` paths or dependency
tree output. These warnings do not block access; they tell the model to switch
to narrower search or grep-style inspection.
