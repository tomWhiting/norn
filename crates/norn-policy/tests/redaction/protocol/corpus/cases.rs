pub(super) struct CorpusCase {
    pub(super) path: &'static str,
    pub(super) bytes: &'static [u8],
}

macro_rules! corpus_case {
    ($path:literal) => {
        CorpusCase {
            path: concat!("crates/norn/testdata/openai_responses/", $path),
            bytes: include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../norn/testdata/openai_responses/",
                $path
            )),
        }
    };
}

pub(super) const CASES: &[CorpusCase] = &[
    corpus_case!("backend-state-matrix.json"),
    corpus_case!("codex/manifest.json"),
    corpus_case!("codex/requests/cache-key-lifecycle.json"),
    corpus_case!("codex/streams/codex-end-turn.sse"),
    corpus_case!("codex/transport/codex-turn-state.json"),
    corpus_case!("codex/transport/config-account-selection.json"),
    corpus_case!("codex/transport/config-auth-source-matrix.json"),
    corpus_case!("codex/transport/oauth-failure-state.json"),
    corpus_case!("codex/transport/oauth-jwt-account-header.json"),
    corpus_case!("codex/transport/oauth-login-durability.json"),
    corpus_case!("codex/transport/oauth-logout-revoke.json"),
    corpus_case!("codex/transport/oauth-named-accounts.json"),
    corpus_case!("codex/transport/oauth-ownership-durability.json"),
    corpus_case!("codex/transport/oauth-refresh-transaction.json"),
    corpus_case!("codex/transport/route-account-affinity-contract.json"),
    corpus_case!("contract-pins.json"),
    corpus_case!("index.json"),
    corpus_case!("public/manifest.json"),
    corpus_case!("public/requests/cache-backend-model-experiment.json"),
    corpus_case!("public/requests/cache-tool-prefix-stability.json"),
    corpus_case!("public/requests/cache-typed-controls.json"),
    corpus_case!("public/requests/request-compatible-roles.json"),
    corpus_case!("public/requests/request-model-profile.json"),
    corpus_case!("public/requests/request-schema-downlevel.json"),
    corpus_case!("public/requests/request-slash-tool-dispatch.json"),
    corpus_case!("public/requests/request-structured-output.json"),
    corpus_case!("public/requests/request-tool-envelopes.json"),
    corpus_case!("public/requests/responses-anchor-reset-reasoning.json"),
    corpus_case!("public/requests/responses-role-authority.json"),
    corpus_case!("public/requests/responses-stateless-replay-order.json"),
    corpus_case!("public/requests/responses-threaded-replacement.json"),
    corpus_case!("public/streams/responses-authoritative-completion.sse"),
    corpus_case!("public/streams/responses-interleaved-duplicate-calls.sse"),
    corpus_case!("public/streams/responses-malformed-terminal.sse"),
    corpus_case!("public/streams/responses-messages-phase-order.sse"),
    corpus_case!("public/streams/responses-refusal.sse"),
    corpus_case!("public/streams/responses-unknown-actionable.sse"),
    corpus_case!("public/streams/responses-web-search-annotations.sse"),
    corpus_case!("public/streams/transport-cancellation.sse"),
    corpus_case!("public/streams/transport-rate-limit-retry.sse"),
    corpus_case!("public/streams/transport-retry-after-ceiling.sse"),
    corpus_case!("public/streams/transport-terminal-once.sse"),
    corpus_case!("public/streams/usage-attempts-and-absence.sse"),
    corpus_case!("public/streams/usage-cache-write.sse"),
];
