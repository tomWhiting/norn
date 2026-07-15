"""Codex overlay and OAuth boundary fixtures backed by pinned Codex source."""

from __future__ import annotations

from responses_fixture_types import (
    CODEX_CLIENT,
    CODEX_COMMON,
    CODEX_LOGIN,
    CODEX_MODELS,
    CODEX_SSE,
    FixtureSpec,
    event,
    response,
    sentinel,
    stream,
    transport,
)


def fixture_specs() -> list[FixtureSpec]:
    return [
        _end_turn(),
        _turn_state(),
        _auth_source_matrix(),
        _jwt_account_header(),
        _refresh_transaction(),
        _failure_state(),
        _login_durability(),
        _logout_revoke(),
        _ownership_durability(),
        _named_accounts(),
        _account_selection(),
        _account_affinity_contract(),
    ]


def _end_turn() -> FixtureSpec:
    finding = "CODEX-01"
    first = {**response(finding), "end_turn": False}
    second = {
        **response(finding),
        "id": sentinel("generic", finding, "response-final"),
        "end_turn": True,
    }
    return stream(
        finding,
        [
            event("response.completed", 1, response=first),
            event("response.completed", 2, response=second),
        ],
        "codex-end-turn-false",
        "codex-end-turn-true",
        "synthetic-robustness",
        dialect="codex",
        sources=(CODEX_COMMON, CODEX_SSE),
    )


def _turn_state() -> FixtureSpec:
    finding = "CODEX-02"
    state = sentinel("state", finding)
    return transport(
        finding,
        {
            "transport": [
                {"type": "response.metadata", "turn_state": state},
                {"type": "response.metadata", "turn_state": None},
            ],
            "requests": [
                {"turn_state": state},
                {"turn_state": state},
                {"turn_state": None},
            ],
        },
        "turn-state-header-receipt",
        "metadata-event-receipt",
        "within-turn-replay",
        "turn-boundary-clear",
        sources=(CODEX_CLIENT, CODEX_SSE, CODEX_COMMON),
    )


def _auth_source_matrix() -> FixtureSpec:
    finding = "CONFIG-01"
    return transport(
        finding,
        {
            "entries": [
                {
                    "name": sentinel("generic", finding, "oauth"),
                    "credential": sentinel("credential", finding, "oauth"),
                    "enabled": True,
                },
                {
                    "name": sentinel("generic", finding, "api-key"),
                    "credential": sentinel("credential", finding, "api-key"),
                    "enabled": True,
                },
            ]
        },
        "typed-auth-source-matrix",
        "selection-before-secret-lookup",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _jwt_account_header() -> FixtureSpec:
    finding = "AUTH-01"
    account = sentinel("account", finding)
    return transport(
        finding,
        {
            "details": [
                {
                    "name": sentinel("generic", finding, "namespaced-claim"),
                    "account_id": account,
                },
                {
                    "name": sentinel("generic", finding, "legacy-claim"),
                    "account_id": account,
                },
            ],
            "chatgpt_account_id": account,
        },
        "namespaced-account-claim",
        "legacy-account-claim",
        "account-header",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _refresh_transaction() -> FixtureSpec:
    finding = "AUTH-02"
    return transport(
        finding,
        {
            "credential": sentinel("credential", finding, "before"),
            "requests": [
                {
                    "reload": True,
                    "lock": True,
                    "refresh": True,
                    "save": True,
                    "credential": sentinel("credential", finding, "after"),
                },
                {
                    "foreign_write": True,
                    "status": "failed",
                    "credential": sentinel("credential", finding, "foreign"),
                },
            ],
        },
        "reload-lock-refresh-save",
        "foreign-write-conflict",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _failure_state() -> FixtureSpec:
    finding = "AUTH-03"
    return transport(
        finding,
        {
            "details": [
                _state(finding, "load", "failed"),
                _state(finding, "parse", "failed"),
                _state(finding, "proactive-refresh", "failed"),
                _state(finding, "stale-token", "incomplete"),
                _state(finding, "unknown-expiry", "queued"),
            ]
        },
        "typed-credential-failure-states",
        "no-absence-collapse",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _state(finding: str, suffix: str, status: str) -> dict[str, object]:
    return {
        "name": sentinel("generic", finding, suffix),
        "status": status,
        "reason": sentinel("generic", finding, f"{suffix}-reason"),
    }


def _login_durability() -> FixtureSpec:
    finding = "AUTH-04"
    return transport(
        finding,
        {
            "entries": [
                {"name": sentinel("generic", finding, "exchange"), "status": "completed"},
                {"name": sentinel("generic", finding, "credential-write"), "status": "completed"},
                {"name": sentinel("generic", finding, "index-write"), "status": "completed"},
                {"name": sentinel("generic", finding, "browser-success"), "status": "completed"},
            ],
            "credential": sentinel("credential", finding),
        },
        "exchange-before-success",
        "durable-credential-and-index",
        sources=(CODEX_LOGIN,),
    )


def _logout_revoke() -> FixtureSpec:
    finding = "AUTH-05"
    return transport(
        finding,
        {
            "entries": [
                {"name": sentinel("generic", finding, "local-delete"), "status": "completed"},
                {"name": sentinel("generic", finding, "remote-revoke"), "status": "failed"},
            ],
            "credential": sentinel("credential", finding),
        },
        "local-delete-independent",
        "separate-revocation-result",
        sources=(CODEX_LOGIN,),
    )


def _ownership_durability() -> FixtureSpec:
    finding = "AUTH-06"
    return transport(
        finding,
        {
            "credential": sentinel("credential", finding, "rotated"),
            "account_id": sentinel("account", finding),
            "entries": [
                {"name": sentinel("generic", finding, "lineage"), "approved": True},
                {"name": sentinel("generic", finding, "durable-owner"), "approved": True},
            ],
        },
        "rotated-lineage-acceptance",
        "durable-explicit-owner",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _named_accounts() -> FixtureSpec:
    finding = "AUTH-07"
    return transport(
        finding,
        {
            "entries": [
                {
                    "name": sentinel("generic", finding, "account-a"),
                    "account_id": sentinel("account", finding, "a"),
                    "approved": True,
                },
                {
                    "name": sentinel("generic", finding, "account-b"),
                    "account_id": sentinel("account", finding, "b"),
                    "approved": True,
                },
            ],
            "enabled": False,
        },
        "named-account-design-target",
        "explicit-selection",
        "no-automatic-rotation",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _account_selection() -> FixtureSpec:
    finding = "CONFIG-02"
    return transport(
        finding,
        {
            "account_id": sentinel("account", finding),
            "credential": sentinel("credential", finding),
            "entries": [
                {"name": sentinel("generic", finding, "source"), "approved": True},
                {"name": sentinel("generic", finding, "provenance"), "approved": True},
                {"name": sentinel("generic", finding, "lifetime-pin"), "approved": True},
            ],
        },
        "typed-account-selection",
        "provider-lifetime-pin",
        sources=(CODEX_CLIENT, CODEX_LOGIN),
    )


def _account_affinity_contract() -> FixtureSpec:
    finding = "ROUTE-01"
    return transport(
        finding,
        {
            "account_id": sentinel("account", finding),
            "turn_state": sentinel("state", finding),
            "enabled": False,
            "entries": [
                {"name": sentinel("generic", finding, "permission"), "approved": False},
                {"name": sentinel("generic", finding, "state-reset"), "approved": False},
            ],
        },
        "automatic-rotation-unsupported",
        "non-execution-contract-target",
        "state-reset-required",
        sources=(CODEX_CLIENT, CODEX_COMMON, CODEX_LOGIN, CODEX_MODELS),
    )
