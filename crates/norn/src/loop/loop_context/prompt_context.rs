use std::time::{Duration, Instant};

use tokio::process::Command;

use crate::context::loader::ContextLoader;
use crate::provider::request::ReasoningEffort;
use crate::system_prompt::environment::format_environment_section;

use super::{DEFAULT_PROMPT_COMMAND_TIMEOUT, LoopContext, PromptCommandCacheEntry};

impl LoopContext {
    /// Reassemble the base system instruction at `system_sections[0]`
    /// from [`Self::base_prefix`], the current
    /// [`ContextLoader::formatted_context`] (when a loader is wired),
    /// and [`Self::base_suffix`].
    ///
    /// Order is fixed: prefix, then always-on context, then suffix —
    /// matching DESIGN.md §D2's layering (Norn base + profile
    /// instructions, then user-level + project-root `NORN.md`, then the
    /// skill catalog listing). Empty parts are skipped so the join does
    /// not produce stray blank lines.
    ///
    /// Pushes a new entry when `system_sections` is empty so callers
    /// can invoke this on a freshly-defaulted [`LoopContext`] without a
    /// separate seeding step.
    pub fn rebuild_base_section(&mut self) {
        let context_body = self
            .context_loader
            .as_ref()
            .map(ContextLoader::formatted_context)
            .unwrap_or_default();
        let parts: [&str; 3] = [
            self.base_prefix.as_str(),
            context_body.as_str(),
            self.base_suffix.as_str(),
        ];
        let assembled: String = parts
            .iter()
            .copied()
            .filter(|s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join("\n\n");
        if let Some(slot) = self.system_sections.first_mut() {
            *slot = assembled;
        } else {
            self.system_sections.push(assembled);
        }
    }

    /// Re-stat the always-on context files and report whether
    /// `system_sections[0]` needs rebuilding.
    ///
    /// Returns `false` when no loader is wired (so callers can invoke
    /// this unconditionally from the iteration top) or when both layers
    /// are unchanged since the last call. Returns `true` when at least
    /// one always-on `NORN.md` layer was added, removed, or rewritten on
    /// disk — at which point the caller (NX-005's iteration wiring)
    /// rebuilds the base instruction from the freshly-loaded
    /// [`ContextLoader::formatted_context`].
    ///
    /// Designed to be called between
    /// [`Self::clear_dynamic_sections`] and
    /// [`Self::evaluate_prompt_commands`] at the start of each iteration
    /// (NX-005 wires the call site in `loop/runner.rs`; this brief only
    /// supplies the method).
    pub fn refresh_context_if_stale(&mut self) -> bool {
        match self.context_loader.as_mut() {
            Some(loader) => loader.check_staleness(),
            None => false,
        }
    }

    /// Join all system sections with a double newline, producing the system
    /// instruction string for the next provider call.
    #[must_use]
    pub fn system_instruction(&self) -> String {
        self.system_sections.join("\n\n")
    }

    /// Return only the base system instruction (index 0), without any
    /// dynamic sections. The base prompt is byte-stable between iterations,
    /// enabling automatic prefix caching on the provider's instructions
    /// field.
    #[must_use]
    pub fn base_system_instruction(&self) -> String {
        self.system_sections.first().cloned().unwrap_or_default()
    }

    /// Collect dynamic sections (indices 1..) into a single string joined
    /// by double newlines. Returns [`None`] when no dynamic sections exist.
    #[must_use]
    pub fn dynamic_context(&self) -> Option<String> {
        if self.system_sections.len() <= 1 {
            return None;
        }
        Some(self.system_sections[1..].join("\n\n"))
    }

    /// Append a dynamic section to the system instruction.
    ///
    /// Dynamic sections live past index 0 and are cleared at the start of
    /// each loop iteration via [`Self::clear_dynamic_sections`].
    pub fn append_system_section(&mut self, content: impl Into<String>) {
        self.system_sections.push(content.into());
    }

    /// Drop all dynamic sections, retaining only the base instruction at
    /// index 0. Called at the top of each loop iteration so rule injections
    /// re-fire fresh.
    pub fn clear_dynamic_sections(&mut self) {
        self.system_sections.truncate(1);
    }

    /// Build the current prompt view over `store`, honouring the active
    /// [`ContextEdits`] when one is installed. Without a tracker, durable
    /// context marks are projected transiently from the store.
    /// Re-append every in-context [`DeliveryMode::SystemContextAppend`] rule's
    /// content to the dynamic system sections from the persisted event
    /// stream.
    ///
    /// Called at the top of each iteration after
    /// [`Self::clear_dynamic_sections`] and before the managed developer
    /// message is synced, so append-mode rule content survives the
    /// per-iteration wipe "for the remainder of the session" — while still
    /// vanishing the instant its
    /// [`SessionEvent::RuleInjection`](crate::session::events::SessionEvent::RuleInjection)
    /// event is compacted or suppressed out of the view (at which point the
    /// rule re-fires on its next trigger). No-op when no rules engine is
    /// installed.
    pub fn materialize_system_context_rules(&mut self, store: &crate::session::store::EventStore) {
        if self.rules.is_none() {
            return;
        }
        let sections: Vec<String> = crate::r#loop::context::with_prompt_context_edits(
            store,
            self.context_edits.as_ref(),
            |edits| {
                store.with_events(|events| {
                    let mut sections = Vec::new();
                    crate::r#loop::context::for_each_visible_event(events, edits, |event, _tag| {
                        if let crate::session::events::SessionEvent::RuleInjection {
                            delivery: crate::rules::types::DeliveryMode::SystemContextAppend,
                            content,
                            ..
                        } = event
                        {
                            sections.push(content.clone());
                        }
                    });
                    sections
                })
            },
        );
        for section in sections {
            self.append_system_section(section);
        }
    }

    /// Rebuild the rules engine's presence set from the current prompt view.
    ///
    /// Invoked immediately before a tool batch's rule evaluation so
    /// `process_event` suppresses rules already present in context and
    /// re-injects only those whose events have been compacted or suppressed
    /// out of the view (N-007 R7). No-op when no rules engine is installed.
    pub fn rebuild_rule_presence(&mut self, store: &crate::session::store::EventStore) {
        if self.rules.is_none() {
            return;
        }
        let tags = crate::r#loop::context::with_prompt_context_edits(
            store,
            self.context_edits.as_ref(),
            |edits| {
                store.with_events(|events| {
                    let mut tags = Vec::new();
                    crate::r#loop::context::for_each_visible_event(events, edits, |_event, tag| {
                        tags.push(tag);
                    });
                    tags
                })
            },
        );
        if let Some(engine) = self.rules.as_mut() {
            engine.presence_mut().rebuild(&tags);
        }
    }

    /// Register nested `NORN.md` synthetic rules for a batch of touched
    /// paths before the rules engine evaluates them (NX-004 / NX-005).
    ///
    /// The [`NestedScanner`](crate::context::scanner::NestedScanner) carries
    /// the immutable launch root captured during assembly. No-op when no
    /// scanner/rules engine is installed or no paths were touched.
    pub fn scan_nested_norn(&mut self, paths: &[String]) {
        if self.rules.is_none() || self.nested_scanner.is_none() || paths.is_empty() {
            return;
        }
        if let (Some(scanner), Some(engine)) = (self.nested_scanner.as_mut(), self.rules.as_mut()) {
            for path in paths {
                scanner.scan_on_path_change(path, engine);
            }
        }
    }

    /// Replace the current reasoning effort with `new_effort`, returning
    /// the prior value so the caller can hand it back to
    /// [`Self::restore_reasoning_effort`] after the activation turn.
    ///
    /// Callers that want to preserve the existing effort (for example
    /// because the activating skill has no `effort` field) must simply
    /// skip the override — calling this method always replaces the
    /// stored value.
    pub fn override_reasoning_effort(
        &mut self,
        new_effort: ReasoningEffort,
    ) -> Option<ReasoningEffort> {
        self.reasoning_effort.replace(new_effort)
    }

    /// Restore the reasoning effort to a previously captured value, as
    /// returned by [`Self::override_reasoning_effort`]. Pass `None` to
    /// clear the field (matching the "no effort hint" baseline).
    pub fn restore_reasoning_effort(&mut self, prior: Option<ReasoningEffort>) {
        self.reasoning_effort = prior;
    }

    /// Append the dynamic `# Environment` section when an
    /// [`EnvironmentConfig`](crate::system_prompt::environment::EnvironmentConfig)
    /// is installed. Gathers current time, working
    /// directory, git branch, and session metadata via Rust APIs (no shell
    /// commands). Called from the runner's iteration top after
    /// [`Self::clear_dynamic_sections`].
    pub fn inject_environment_section(&mut self) {
        if let Some(config) = &self.environment {
            let working_dir = self.working_dir.get();
            let section = format_environment_section(config, &working_dir);
            self.append_system_section(section);
        }
    }

    /// Append the dynamic `# Collaboration Mode` section based on
    /// the current [`CollaborationMode`](crate::system_prompt::builder::CollaborationMode).
    /// Called from the runner's
    /// iteration top after [`Self::clear_dynamic_sections`].
    pub fn inject_collaboration_mode(&mut self) {
        let section = self.collaboration_mode.format_section();
        self.append_system_section(section);
    }

    /// Evaluate every registered [`PromptCommand`](crate::profile::PromptCommand)
    /// and append a dynamic
    /// system section per success. Failures (non-zero exit, spawn error,
    /// timeout) are logged via `tracing::warn!` and skipped — the loop
    /// continues without that section.
    ///
    /// Cache misses run **concurrently**, each under `timeout` (`None`
    /// defers to [`DEFAULT_PROMPT_COMMAND_TIMEOUT`], the documented
    /// default; the runner passes
    /// [`AgentLoopConfig::prompt_command_timeout`](crate::agent_loop::config::AgentLoopConfig::prompt_command_timeout)),
    /// so an iteration's prompt-command wall-clock cost is the slowest
    /// command, not the sum. Sections append in registration order
    /// regardless of completion order.
    ///
    /// Callers must invoke this method at the start of every iteration
    /// after [`Self::clear_dynamic_sections`] so the dynamic sections live
    /// for exactly the next provider call.
    pub async fn evaluate_prompt_commands(&mut self, timeout: Option<Duration>) {
        if self.prompt_commands.is_empty() {
            return;
        }
        let timeout = timeout.unwrap_or(DEFAULT_PROMPT_COMMAND_TIMEOUT);
        let commands = self.prompt_commands.clone();
        let now = Instant::now();
        // Resolve cache hits up front; only misses spend a subprocess.
        let cached: Vec<Option<String>> = commands
            .iter()
            .map(|cmd| {
                self.prompt_command_cache
                    .get(&cmd.name)
                    .filter(|entry| entry.expires_at.is_some_and(|deadline| deadline > now))
                    .map(|entry| entry.value.clone())
            })
            .collect();

        let working_dir = self.working_dir.get();
        let misses: Vec<_> = commands
            .iter()
            .zip(&cached)
            .filter(|(_, cached_value)| cached_value.is_none())
            .map(|(cmd, _)| run_prompt_command(&cmd.command, &working_dir, timeout))
            .collect();
        let mut miss_results = futures_util::future::join_all(misses).await.into_iter();

        for (cmd, cached_value) in commands.iter().zip(cached) {
            let outcome = match cached_value {
                Some(value) => Ok(value),
                None => match miss_results.next() {
                    Some(result) => result,
                    // Structurally unreachable: one future was created per
                    // cache miss, in the same order this loop consumes.
                    None => Err("concurrent evaluation produced no result".to_owned()),
                },
            };
            match outcome {
                Ok(stdout) => {
                    if let Some(ttl) = cmd.cache_ttl {
                        self.prompt_command_cache.insert(
                            cmd.name.clone(),
                            PromptCommandCacheEntry {
                                value: stdout.clone(),
                                expires_at: Some(now + ttl),
                            },
                        );
                    } else {
                        // No TTL means caching is disabled; drop any stale entry
                        // so we never accidentally hit it later.
                        self.prompt_command_cache.remove(&cmd.name);
                    }
                    self.append_system_section(format_section(&cmd.name, &stdout));
                }
                Err(err) => {
                    tracing::warn!(
                        command = %cmd.name,
                        error = %err,
                        "prompt command failed; skipping section",
                    );
                }
            }
        }
    }
}

fn format_section(name: &str, body: &str) -> String {
    format!("# {name}\n{body}")
}

async fn run_prompt_command(
    command: &str,
    working_dir: &std::path::Path,
    timeout: Duration,
) -> Result<String, String> {
    let governor = crate::resource::DescriptorGovernor::global()
        .map_err(|error| format!("prompt command descriptor admission unavailable: {error}"))?;
    let _permit = governor
        .try_acquire(crate::resource::TWO_PIPE_SPAWN_PEAK)
        .map_err(|error| format!("prompt command descriptor admission failed: {error}"))?;
    let result = tokio::time::timeout(
        timeout,
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .kill_on_drop(true)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_owned())
        }
        Ok(Ok(output)) => {
            let exit = output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(format!("prompt command exited {exit}: {stderr}"))
        }
        Ok(Err(e)) => Err(format!("failed to spawn prompt command: {e}")),
        Err(_) => Err(format!("prompt command timed out after {timeout:?}")),
    }
}
