Use for multi-hunk or multi-file changes that are easier to express as a unified diff than as individual edit calls. Prefer edit for single-site replacements. The patch must use standard unified-diff format with --- and +++ headers. Each hunk header must be `@@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@` (an optional anchor such as a function name may follow); a bare `@@` is rejected. Minimal example:

--- a/src/lib.rs
+++ b/src/lib.rs
@@ -2,1 +2,1 @@
-    let x = 1;
+    let x = 2;

Git-style a/ and b/ prefixes are stripped automatically. All target files must have been read this session. Use working_dir to resolve relative paths in the patch against a specific directory. Patches are validated with tree-sitter: syntax errors roll back all files atomically (gate semantics) unless AllowBrokenAst is set.