Use to check on agents you spawned or forked: after launching a child, before retrying a signal, before closing, or whenever you need to know whether a child is still running. The view is scoped to yourself and your descendant subtree — your parent and siblings are not visible, and asking for one returns a permission_denied failure.

list returns every visible agent. A live agent carries id, path, role, model, status (spawning, active, completing, completed, failed), parent_id, spawned_at, completed_at once terminal, and "reclaimed": false; your own entry is marked "self": true. A descendant that finished and was reclaimed appears from its completion record, marked "reclaimed": true with id, path, terminal status, parent_id, and completed_at — role, model, and spawn time are not retained after reclamation.

get takes agent_id — a hierarchical registry path (e.g. "/workers/analyzer") or UUID — and returns the same record shape. A child that finished but was not yet reclaimed reports its full entry with its real terminal status; a reclaimed child reports its completion record. not_found is returned only for identifiers no agent in this session ever had — a finished agent always resolves to its record.

Both commands are read-only: nothing is signalled, closed, or reclaimed by looking.
