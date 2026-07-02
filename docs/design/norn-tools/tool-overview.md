# Tools

<!-- NOTES (Waffles, 2026-05-26):
  - VCS renamed to Source throughout per Tom's direction — more approachable
  - MeridianProject removed for now — not set up yet
  - InteractiveMessagePattern at the bottom is key — replaces dedicated
    review/approval tools with a general structured-message/respond pattern
  - TODO: think about how signal_agent, close_agent, and fork could integrate
    with the Meridian namespace tools (e.g., can an agent signal a sub-agent
    AND send it a DM through messaging? should spawn_agent accept a Meridian
    profile that includes namespace tools?)
  - TODO: the follow-on suggestions are prescriptive — consider whether this
    should be guidance in the system prompt rather than hardcoded in tool
    definitions. The model should discover follow-ons, not be told them.
  - RESOLVED: meridian_admin dissolved. Three-tool split per Tom:
    meridian_member (identity), meridian_workspace (environment),
    meridian_exchange (cross-instance). Channel ops → messaging,
    scheduler → workflow, workspace ops → workspace, profile → member.
-->

## FileSystem
- apply_patch
  - Input Parameters
    - patch
    - working_dir
    - mode
    - tool_use_description
    - tool_use_metadata
  - Output
    - patch_applied: files changed with line counts
      - read: path={changed_file}, offset={changed_region}
      - bash: command="cargo check" (verify compilation)
      - search: pattern={function_name}, path={changed_file} (verify context)
    - patch_failed_context_mismatch: file content diverged from patch expectations
      - read: path={target_file}, offset={expected_region} (inspect current state)
      - search: pattern={missing_context_line}, path={target_file} (find where content moved)
    - patch_failed_file_not_found: target file does not exist
      - search: pattern={filename}, mode=glob (find correct path)
      - read: path={directory} (list directory contents)
    - patch_failed_permission_denied: filesystem write blocked
      - bash: command="ls -la {path}" (check permissions)
    - patch_partial_applied: some hunks succeeded, others failed
      - read: path={target_file} (inspect result)
      - edit: path={target_file}, old_string={failed_region} (manual fix)

- write
  - Input Parameters
    - path
    - content
    - tool_use_description
    - tool_use_metadata
  - Output
    - file_written: bytes written, path confirmed
      - read: path={written_file}, limit=20 (verify content)
      - bash: command="cargo check" (verify compilation if .rs)
      - edit: path={written_file} (make corrections)
    - write_failed_permission_denied: filesystem write blocked
      - bash: command="ls -la {parent_dir}" (check permissions)
    - write_failed_directory_not_found: parent directory does not exist
      - bash: command="mkdir -p {parent_dir}" (create directory)
      - write: path={path}, content={content} (retry)
    - write_blocked_by_convention: diagnostics advisory/block fired
      - read: path={conventions_file} (check which convention triggered)

- edit
  - Input Parameters
    - path
    - old_string
    - new_string
    - occurrence
    - tool_use_description
    - tool_use_metadata
  - Output
    - edit_applied: replacement made, line range affected
      - read: path={file}, offset={affected_line} (verify in context)
      - bash: command="cargo check" (verify compilation if .rs)
      - edit: path={file} (chain another edit in same file)
    - edit_failed_string_not_found: old_string not present in file
      - read: path={file} (inspect current content)
      - search: pattern={old_string_fragment}, path={file} (find similar text)
    - edit_failed_multiple_matches: old_string matches more than once and no occurrence specified
      - search: pattern={old_string}, path={file}, mode=count (see how many matches)
      - edit: path={file}, old_string={longer_context}, new_string={replacement} (use more context)
    - edit_failed_file_not_found: file does not exist
      - search: pattern={filename}, mode=glob (find correct path)
      - write: path={path}, content={new_content} (create file instead)
    - edit_blocked_by_convention: diagnostics advisory/block fired
      - read: path={conventions_file} (check which convention triggered)

- read
  - Input Parameters
    - path
    - offset
    - limit
    - tool_use_description
    - tool_use_metadata
  - Output
    - file_content: line-numbered text, total_lines, language detected
      - edit: path={file}, old_string={line_to_change} (modify what you read)
      - search: pattern={symbol}, path={file} (find references within file)
      - read: path={file}, offset={next_offset} (continue reading)
    - file_not_found: path does not exist
      - search: pattern={filename}, mode=glob (find correct path)
    - file_empty: file exists but has zero content
      - write: path={file}, content={initial_content} (populate empty file)
    - file_is_directory: path is a directory, not a file
      - bash: command="ls {path}" (list directory contents)
    - file_too_large_no_range: file exceeds display limit without offset/limit
      - read: path={file}, offset=0, limit=100 (read first section)
      - search: pattern={target}, path={file} (find specific content)
    - binary_file: file is not text
      - bash: command="file {path}" (check file type)

## Shell
- bash
  - Input Parameters
    - command
    - timeout
    - working_dir
    - tool_use_description
    - tool_use_metadata
  - Output
    - command_success: exit_code=0, stdout, stderr
      - bash: command={next_command} (chain commands)
      - read: path={output_file} (read generated output)
      - edit: path={source_file} (fix issues surfaced by command)
    - command_failed: exit_code!=0, stdout, stderr
      - bash: command={diagnostic_command} (investigate failure)
      - read: path={error_source_file} (inspect referenced file)
      - search: pattern={error_message}, path=. (find error source)
      - edit: path={source_file}, old_string={broken_code} (fix the issue)
    - command_timeout: execution exceeded timeout
      - bash: command={command}, timeout={longer_timeout} (retry with more time)
      - bash: command="ps aux | grep {process}" (check for stuck processes)
    - command_blocked: command rejected by shell safety policy
      - bash: command={safer_alternative} (use approved command)

## Search
- search
  - Input Parameters
    - pattern
    - path
    - glob
    - max_results
    - mode
    - ast_query
    - tool_use_description
    - tool_use_metadata
  - Output
    - matches_found: list of file:line:content matches
      - read: path={matched_file}, offset={matched_line} (read context around match)
      - edit: path={matched_file}, old_string={matched_line} (modify match)
      - search: pattern={refined_pattern}, path={matched_file} (narrow search)
    - no_matches: zero results for the query
      - search: pattern={broader_pattern} (broaden search)
      - search: pattern={alternative_spelling} (try alternate names)
      - search: mode=glob, pattern={filename_pattern} (search by filename instead)
    - too_many_matches: results capped at max_results
      - search: pattern={pattern}, glob={narrower_glob} (add file filter)
      - search: pattern={more_specific_pattern} (refine regex)
    - invalid_pattern: regex syntax error
      - search: pattern={escaped_pattern} (fix regex escapes)
    - ast_query_no_language: AST mode requested but language not detectable
      - search: pattern={pattern}, glob="*.{ext}" (add file extension filter)

## Development
- lsp
  - Input Parameters
    - action
    - path
    - line
    - column
    - symbol
    - tool_use_description
    - tool_use_metadata
  - Output
    - definitions_found: list of location(file, line, column) for go-to-definition
      - read: path={definition_file}, offset={definition_line} (read the definition)
    - references_found: list of location(file, line) for find-references
      - read: path={reference_file}, offset={reference_line} (read each reference site)
      - edit: path={reference_file} (modify a reference)
    - hover_info: type signature, documentation for symbol at position
      - read: path={file}, offset={line} (read surrounding context)
    - diagnostics_found: list of errors/warnings with file, line, message
      - edit: path={diagnostic_file}, old_string={error_line} (fix the error)
      - read: path={diagnostic_file}, offset={error_line} (read context)
    - completions_found: list of completion items with labels and kinds
      - edit: path={file} (apply a completion)
    - symbol_not_found: no results for the query
      - search: pattern={symbol} (fall back to text search)
    - lsp_unavailable: language server not running or not configured
      - search: pattern={symbol} (fall back to text search)

## Web
- web_search
  - Input Parameters
    - query
    - num_results
    - tool_use_description
    - tool_use_metadata
  - Output
    - results_found: list of title, url, snippet
      - web_fetch: url={result_url} (read full page content)
    - no_results: query returned nothing
      - web_search: query={rephrased_query} (try alternate phrasing)
    - search_error: API failure or rate limit
      - web_search: query={query} (retry)

- web_fetch
  - Input Parameters
    - url
    - format
    - timeout
    - tool_use_description
    - tool_use_metadata
  - Output
    - page_content: extracted text/markdown from the URL
      - web_fetch: url={linked_url} (follow a link from the page)
      - search: pattern={relevant_term} (find related content in codebase)
    - fetch_failed_network: connection error, DNS failure, timeout
      - web_fetch: url={url}, timeout={longer_timeout} (retry with more time)
    - fetch_failed_http_error: 4xx/5xx status code
      - web_search: query="{url_domain} {topic}" (search for alternative source)
    - fetch_failed_invalid_url: URL scheme not http/https or malformed
      - web_search: query={topic} (search instead of fetch)

## TaskManagement
- task
  - Input Parameters
    - action
    - task_id
    - description
    - status
    - depends_on
    - metadata
    - parent_task_id
    - agent_path
    - group_slug
    - tool_use_description
    - tool_use_metadata
  - Output
    - task_created: task_id, description
      - task: action=update, task_id={new_id}, status=in_progress (start working on it)
      - task: action=create, parent_task_id={new_id} (create subtask)
    - task_updated: task_id, new_status
      - task: action=list (see remaining work)
      - task: action=create (add follow-up task)
    - task_list: list of tasks with id, description, status, dependencies
      - task: action=update, task_id={next_pending}, status=in_progress (pick up next task)
      - read: path={relevant_file} (start working on a task)
    - task_not_found: task_id does not exist
      - task: action=list (see available tasks)
    - task_dependency_cycle: update would create circular dependency
      - task: action=list (inspect dependency graph)

## Agent
- fork
  - Input Parameters
    - request
    - model
    - requirements
    - tool_use_description
    - tool_use_metadata
  - Output
    - fork_created: agent_id, forked from current conversation state
      - signal_agent: to={agent_id}, content={additional_context} (send more info)
      - task: action=create, description="Waiting for fork {agent_id}" (track the fork)
    - fork_failed: error creating child agent
      - spawn_agent: task={request} (try spawn instead of fork)

- spawn_agent
  - Input Parameters
    - task
    - model
    - role
    - profile
    - tools
    - path
    - tool_use_description
    - tool_use_metadata
  - Output
    - agent_spawned: agent_id, task assigned
      - signal_agent: to={agent_id}, content={clarification} (send additional context)
      - task: action=create, description="Waiting for agent {agent_id}" (track the agent)
    - spawn_failed: error creating agent (profile not found, resource limit)
      - spawn_agent: task={task}, profile={default_profile} (retry with default profile)

- signal_agent
  - Input Parameters
    - to
    - content
    - trigger_turn
    - tool_use_description
    - tool_use_metadata
  - Output
    - signal_delivered: agent_id acknowledged
      - task: action=list (continue own work while waiting)
    - signal_failed_agent_not_found: target agent_id doesn't exist or completed
      - task: action=list (check if agent's task is done)
      - action_log: query="agent_id={to}" (check what happened to the agent)
    - signal_failed_channel_closed: agent's inbound channel is closed
      - close_agent: agent_id={to} (clean up the dead agent)

- close_agent
  - Input Parameters
    - agent_id
    - reason
    - tool_use_description
    - tool_use_metadata
  - Output
    - agent_closed: agent_id, final result if available
      - task: action=update, status=completed (mark coordinating task done)
      - read: path={agent_output_file} (review agent's work)
    - agent_not_found: agent_id doesn't exist
      - action_log: query="agent_id={agent_id}" (check history)

- tool_search
  - Input Parameters
    - query
    - max_results
    - tool_use_description
    - tool_use_metadata
  - Output
    - tools_found: list of tool name, description, input_schema
      - {matched_tool}: (invoke the discovered tool with appropriate params)
    - no_matches: no tools match the query
      - tool_search: query={broader_query} (broaden search)

## Discovery
- action_log
  - Input Parameters
    - query
    - filter
    - call_id
    - tool_use_description
    - tool_use_metadata
  - Output
    - entries_found: list of action log entries with timestamps, tool, result
      - read: path={referenced_file} (inspect a file mentioned in an entry)
      - action_log: filter={narrower_filter} (drill down)
    - no_entries: no log entries match the query
      - action_log: query={broader_query} (broaden search)
    - entry_detail: single entry with full input/output for a call_id
      - read: path={file_from_entry} (inspect the file that was acted on)

## Skills
- skill
  - Input Parameters
    - name
    - arguments
    - tool_use_description
    - tool_use_metadata
  - Output
    - skill_loaded: skill content injected into conversation as system context
      - (skill-dependent: follow the skill's instructions)
    - skill_not_found: named skill doesn't exist
      - tool_search: query="skill {name}" (search for similar tools/skills)
    - skill_error: skill template failed to render
      - skill: name={name}, arguments={corrected_args} (retry with fixed arguments)

## MeridianMessaging
- meridian_messaging
  - Input Parameters
    - command: send | read | inbox | respond | snooze | notify_check | notify_summary | search | mark_read | channel_send | channel_mentions | retry
    - to (send, channel_send): recipient name or channel name
    - message (send, channel_send, respond): message content
    - subject (send): optional subject line
    - message_id (read, snooze, respond, retry, mark_read): target message UUID
    - choice (respond): response option key from interactive message
    - query (search): search text
    - limit (inbox, search, channel_mentions): max results
    - dm_only (inbox): filter to DMs only
    - channel (channel_send, channel_mentions): channel name
    - tool_use_description
    - tool_use_metadata
  - Output
    - send_success { message_id, recipient, delivered_at }
      - read { message_id } — verify delivery by reading the sent message
      - action_log { filter: "sent" } — view recent send history
    - send_partial { delivered: [...], failed: [...] } (multi-recipient)
      - retry { message_id } — retry the failed recipients
      - read { message_id } — check a delivered message
    - send_error { error, recipient }
      - meridian_member { command: "lookup", name: recipient } — verify recipient exists
    - inbox_result { items: [{ message_id, sender, subject, preview, urgency, unread, created_at }], total }
      - read { message_id: items[0].message_id } — read the most urgent/newest unread
      - mark_read { partner: items[0].sender } — mark conversation read if triaging
      - snooze { message_id: items[0].message_id } — snooze a low-priority item
    - inbox_empty {}
      - action_log { filter: "received" } — check if messages were already handled
    - read_result { message_id, sender, content, subject, created_at, response_options, thread_id }
      - respond { message_id, choice } — when response_options present (interactive message)
      - meridian_messaging { command: "send", to: sender, message: "..." } — reply to sender
      - mark_read { partner: sender } — mark conversation as read
      - snooze { message_id } — defer for later
    - respond_success { message_id, choice, response_delivered }
      - inbox {} — return to inbox for next item
      - action_log { filter: "responded" } — confirm response logged
    - notify_check_result { count, has_urgent }
      - notify_summary {} — get grouped details when count > 0
      - inbox { dm_only: true } — jump to DMs if urgent
    - notify_summary_result { groups: [{ source, count, latest }] }
      - inbox {} — process DMs
      - read { message_id: groups[0].latest.message_id } — read latest in top group
    - search_result { matches: [{ message_id, sender, preview, score }] }
      - read { message_id: matches[0].message_id } — read the top result
    - snooze_success { message_id, snoozed_until }
      - inbox {} — continue triaging
    - mark_read_success { partner }
      - inbox {} — continue triaging
    - channel_send_success { channel, message_id }
      - channel_mentions { channel } — check for responses
    - channel_mentions_result { mentions: [{ message_id, sender, content, channel }] }
      - read { message_id: mentions[0].message_id } — read top mention
      - channel_send { channel, message: "..." } — reply in channel
    - error { code, message, command_attempted }
      - (varies by code — member_not_found suggests meridian_member lookup, message_not_found suggests inbox refresh)

## MeridianMember
- meridian_member
  - Input Parameters
    - command: get | list | lookup | status_set | status_get | activity
    - name (get, lookup): member name to find
    - member_id (get, status_get): member UUID
    - status (status_set): activity status (active, available, busy)
    - focus (activity): focus text describing current work
    - workspace_id (list): optional workspace filter
    - tool_use_description
    - tool_use_metadata
  - Output
    - get_result { member_id, name, kind, activity, focus, last_seen, workspace, reporting }
      - meridian_messaging { command: "send", to: name } — send a message to this member
      - status_get { member_id } — refresh their status
    - list_result { members: [{ member_id, name, kind, activity, focus }] }
      - get { name: members[N].name } — get full details on a specific member
      - meridian_messaging { command: "send", to: members[N].name } — message a member
    - lookup_result { member_id, name }
      - get { member_id } — get full details
      - meridian_messaging { command: "send", to: name } — message them
    - lookup_not_found { name, suggestions: [...] }
      - lookup { name: suggestions[0] } — try a suggested name
      - list {} — browse all members
    - status_set_success { member_id, new_status }
      - activity { focus: "..." } — set focus text to explain the status change
    - status_get_result { member_id, name, activity, focus, last_seen }
      - meridian_messaging { command: "send", to: name } — message them based on their status
    - activity_success { member_id, focus }
      - inbox {} — check for pending work now that status is updated
    - error { code, message, command_attempted }

## MeridianSource
- meridian_source
  - Input Parameters
    - command: status | diff | log | blame | stage | unstage | commit | push | pull | branches
    - path (diff, blame, stage, unstage): file path
    - message (commit): commit message
    - ref_spec (diff, log): git ref or range
    - limit (log): max commits
    - tool_use_description
    - tool_use_metadata
  - Output
    - status_result { branch, staged: [...], unstaged: [...], untracked: [...] }
      - diff {} — see what changed in detail
      - stage { path: unstaged[0] } — stage a modified file
      - commit { message: "..." } — commit if staged files ready
    - diff_result { files: [{ path, hunks }], stats }
      - edit { path: files[0].path } — fix something in the diff
      - stage { path: files[0].path } — stage after reviewing
      - commit { message: "..." } — commit the changes
    - log_result { commits: [{ hash, author, message, date }] }
      - diff { ref_spec: commits[0].hash } — see a specific commit's changes
    - blame_result { lines: [{ line, hash, author, date }] }
      - log { ref_spec: lines[0].hash } — see the commit that introduced this line
      - meridian_member { command: "get", name: lines[0].author } — look up the author
    - stage_success { path }
      - status {} — check what's now staged
      - commit { message: "..." } — commit if ready
    - commit_success { hash, message }
      - push {} — push to remote
      - log { limit: 1 } — verify the commit
      - meridian_branch { command: "status" } — check branch state
    - push_success { remote, branch, commits_pushed }
      - meridian_branch { command: "submit" } — create PR if ready
      - log { limit: commits_pushed } — review what was pushed
    - push_error { error, remote }
      - pull {} — pull first if behind
      - status {} — check for conflicts
    - pull_result { updated_files, conflicts: [...] }
      - status {} — see current state after pull
      - diff {} — review pulled changes
    - branches_result { current, branches: [{ name, upstream, ahead, behind }] }
      - meridian_branch { command: "status" } — detailed branch lifecycle state
    - error { code, message, command_attempted }

## MeridianBranch
- meridian_branch
  - Input Parameters
    - command: status | submit | land | transition | list | show | advance
    - branch (show, transition, land): branch name
    - title (submit): PR title
    - body (submit): PR description
    - tool_use_description
    - tool_use_metadata
  - Output
    - status_result { branch, lifecycle_state, pr_url, review_status, checks_status }
      - submit { title: "..." } — submit if not yet submitted
      - land {} — land if approved and checks pass
      - meridian_source { command: "log", limit: 5 } — review recent commits before submitting
    - submit_success { branch, pr_url, pr_number }
      - status {} — check submission state
      - meridian_messaging { command: "send", to: "reviewer", message: "PR ready..." } — notify reviewer
    - land_success { branch, merged_to, pr_number }
      - transition { branch: "next-branch" } — move to next branch in stack
      - status {} — confirm landed state
      - meridian_exchange { command: "workspace_list" } — notify shared workspace participants if in shared mode
    - land_error { error, branch, pr_status, checks }
      - status {} — diagnose what's blocking
      - meridian_source { command: "log", limit: 3 } — check recent state
    - transition_success { from_branch, to_branch }
      - status {} — check new branch's state
      - meridian_source { command: "status" } — see working tree state
    - list_result { branches: [{ name, lifecycle_state, pr_url }] }
      - show { branch: branches[N].name } — get details on a specific branch
    - show_result { branch, lifecycle_state, pr_url, review_status, checks, commits }
      - transition { branch } — switch to this branch
    - advance_success { branch, new_state }
      - status {} — confirm the transition
    - error { code, message, command_attempted }

## MeridianWorkflow
- meridian_workflow
  - Input Parameters
    - command: list | run | status | cancel
    - workflow (run): workflow name
    - params (run): workflow parameters as JSON
    - execution_id (status, cancel): execution UUID
    - tool_use_description
    - tool_use_metadata
  - Output
    - list_result { workflows: [{ name, description, params_schema }] }
      - run { workflow: workflows[N].name } — dispatch a listed workflow
    - run_success { execution_id, workflow, status }
      - status { execution_id } — check progress
    - status_result { execution_id, workflow, status, steps: [{ name, status, output }], duration }
      - cancel { execution_id } — cancel if stuck or unwanted
      - status { execution_id } — refresh status if still running
    - cancel_success { execution_id }
      - list {} — see available workflows
    - error { code, message, command_attempted }

<!-- MeridianProject: deferred per Tom — project management tools not set up yet -->

## MeridianExchange
- meridian_exchange
  - Input Parameters
    - command: activate | deactivate | status | identity | contracts_list | contracts_get | contracts_propose | contracts_accept | contracts_revoke | peers_list | peers_get | audit | connect | propose | send | workspace_propose | workspace_accept | workspace_teardown | workspace_list | workspace_add_participant | workspace_remove_participant | workspace_focus | workspace_unfocus
    - contract_id (contracts_get, contracts_accept, contracts_revoke, audit): contract UUID
    - peer_id (peers_get): peer instance ID
    - terms (contracts_propose, propose): contract terms JSON
    - peer_url (propose): target instance URL or connection code
    - message (send): message content for Tier 1 delivery
    - member (send): target member designation (member@instance)
    - workspace_id (workspace_accept, workspace_teardown, workspace_list, workspace_add_participant, workspace_remove_participant): workspace UUID
    - instance_id (workspace_add_participant, workspace_remove_participant): participant instance ID
    - code (connect): connection code to resolve
    - filter (audit): audit query filter (peer_id, event_type, date range)
    - tool_use_description
    - tool_use_metadata
  - Output
    - activate_success { instance_id, public_key }
      - status {} — verify exchange is active
      - identity {} — inspect public key and instance ID
    - deactivate_success {}
      - status {} — confirm deactivated
    - status_result { enabled, instance_id, connected_peers, active_contracts }
      - contracts_list {} — view contract details
      - peers_list {} — view peer details
      - identity {} — inspect identity
    - identity_result { instance_id, public_key, relay_url }
      - status {} — check connection health
    - contracts_list_result { contracts: [{ contract_id, peer_id, state, capabilities }] }
      - contracts_get { contract_id: contracts[N].contract_id } — view full terms
      - workspace_propose { contract_id: contracts[N].contract_id } — propose shared workspace
    - contracts_get_result { contract_id, peer_id, state, our_terms, their_terms, established_at, expires_at }
      - contracts_accept { contract_id } — accept if state=proposed
      - contracts_revoke { contract_id } — revoke if no longer wanted
      - audit { contract_id } — view activity under this contract
      - workspace_list { contract_id } — view workspaces governed by this contract
    - contracts_propose_success { proposal_id, peer_id, expires_at }
      - status {} — check for response
      - meridian_messaging { command: "inbox" } — wait for acceptance notification
    - contracts_accept_success { contract_id, peer_id }
      - workspace_propose { contract_id } — propose shared workspace on this contract
      - meridian_messaging { command: "send", to: peer_id, message: "..." } — notify peer
    - contracts_revoke_success { contract_id }
      - contracts_list {} — view remaining contracts
      - meridian_messaging { command: "send", to: peer_id, message: "..." } — notify affected parties
    - peers_list_result { peers: [{ peer_id, health, last_seen, transport }] }
      - peers_get { peer_id: peers[N].peer_id } — view peer details
    - peers_get_result { peer_id, public_key, health, relay_url, direct_url, contracts }
      - contracts_get { contract_id: contracts[N] } — view contract with this peer
      - meridian_messaging { command: "send", to: "member@{peer_id}", contract_id: contracts[N] } — message a member on this peer
    - audit_result { entries: [{ event_type, peer_id, contract_id, timestamp, details }] }
      - contracts_get { contract_id: entries[N].contract_id } — view contract context
      - audit { filter: { event_type: entries[N].event_type } } — drill into specific event type
    - connect_success { peer_id, instance_id }
      - propose { peer_url: peer_id, terms: {...} } — propose exchange to resolved peer
    - propose_success { proposal_id }
      - status {} — check for acceptance
    - send_success { message_id, contract_id, recipient }
      - meridian_messaging { command: "inbox" } — check for response
      - audit { contract_id } — view message in audit trail
    - workspace_propose_success { workspace_id, contract_id, state: "proposed" }
      - workspace_list {} — check workspace status
    - workspace_accept_success { workspace_id, contract_id, state: "provisioning" }
      - workspace_list {} — monitor provisioning progress
      - meridian_messaging { command: "send" } — notify participants
    - workspace_teardown_success { workspace_id, state: "terminated" }
      - contracts_revoke { contract_id } — revoke governing contract if no longer needed
      - contracts_list {} — view remaining contracts
      - meridian_messaging { command: "send" } — notify affected participants
    - workspace_list_result { workspaces: [{ workspace_id, contract_id, state, participants }] }
      - workspace_add_participant { workspace_id, instance_id } — add a participant (creator only)
      - workspace_remove_participant { workspace_id, instance_id } — remove a participant
      - workspace_teardown { workspace_id } — tear down workspace (creator only)
    - workspace_add_participant_success { workspace_id, instance_id }
      - workspace_list {} — verify updated participant list
      - meridian_messaging { command: "send" } — notify new participant
    - workspace_remove_participant_success { workspace_id, instance_id }
      - workspace_list {} — verify updated participant list
    - workspace_focus_success { workspace_id, mount_path }
      - meridian_source { command: "status" } — see shared workspace source state
      - meridian_branch { command: "list" } — see shared workspace branches
    - workspace_unfocus_success {}
      - meridian_source { command: "status" } — see local workspace source state
    - workspace_focus_error { workspace_id, reason: "workspace_not_active" }
      - workspace_list {} — check workspace state
    - error { code, message, command_attempted }

## MeridianWorkspace
- meridian_workspace
  - Input Parameters
    - command: workspace_list | workspace_get | workspace_members | workspace_config
    - workspace_id (workspace_get, workspace_members, workspace_config): workspace UUID
    - tool_use_description
    - tool_use_metadata
  - Output
    - workspace_list_result { workspaces: [{ workspace_id, name, member_count, mode }] }
      - workspace_get { workspace_id: workspaces[N].workspace_id } — view workspace details
      - workspace_members { workspace_id: workspaces[N].workspace_id } — see who's in it
    - workspace_get_result { workspace_id, name, config, mode, created_at }
      - workspace_members { workspace_id } — list workspace members
      - workspace_config { workspace_id } — view configuration
      - meridian_exchange { command: "contracts_list" } — view exchange contracts for this workspace
      - meridian_exchange { command: "workspace_list" } — view shared workspace status if federated
    - workspace_members_result { workspace_id, members: [{ member_id, name, role, status }] }
      - meridian_member { command: "get", member_id: members[N].member_id } — view member details
      - meridian_messaging { command: "send", to: members[N].name } — message a member
    - workspace_config_result { workspace_id, config: { ... } }
      - workspace_get { workspace_id } — see full workspace details
    - error { code, message, command_attempted }

## InteractiveMessagePattern

Cross-cutting pattern connecting messaging, follow-on actions, and the action log.

When a workflow step needs agent input (review verdict, approval, structured decision), it sends
an interactive message with:
- content: the request description
- response_options: keyed choices with labels (e.g., approve | reject | request_changes)
- response_schema: optional JSON schema for structured response data beyond the choice key

The agent reads the message via meridian_messaging command=read, which returns response_options
and schema. The natural follow-on is meridian_messaging command=respond with message_id, choice,
and optional structured comment/data matching the schema. The response is delivered back to the
workflow step as structured output.

The action_log tracks:
- message_received { message_id, sender, subject, has_response_options, timestamp }
- message_read { message_id, timestamp }
- response_sent { message_id, choice, structured_data, timestamp }
- pending_responses: messages with has_response_options=true that have message_received but no response_sent

Follow-on from action_log when pending_responses exist:
- meridian_messaging { command: "read", message_id: pending[0].message_id }
  then meridian_messaging { command: "respond", message_id, choice }

This replaces dedicated review/approval tools with a general pattern: workflow sends structured
message, agent responds with structured output, action log tracks completion.
