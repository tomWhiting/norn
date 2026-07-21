use super::*;

fn retryable(kind: TransientKind) -> ErrorClass {
    ErrorClass::Retryable { kind }
}

// -- ProviderError: pinned classification per variant -------------------

#[test]
fn connection_failed_timeout_classifies_retryable_timeout() {
    let err = ProviderError::ConnectionFailed {
        reason: "no response headers within 30.0s".to_string(),
        kind: TransientKind::Timeout,
    };
    assert_eq!(err.class(), retryable(TransientKind::Timeout));
    assert!(err.is_retryable());
}

#[test]
fn connection_failed_reset_classifies_retryable_reset() {
    let err = ProviderError::ConnectionFailed {
        reason: "connection refused".to_string(),
        kind: TransientKind::ConnectionReset,
    };
    assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
}

/// The reason text carries zero classification weight: a reason that
/// *mentions* a timeout still classifies by its structured kind.
#[test]
fn connection_failed_classifies_by_structured_kind_not_reason_text() {
    let err = ProviderError::ConnectionFailed {
        reason: "request timed out after 30s".to_string(),
        kind: TransientKind::ConnectionReset,
    };
    assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
}

#[test]
fn stream_interrupted_classifies_retryable_reset() {
    let err = ProviderError::StreamInterrupted {
        reason: "body ended mid-stream".to_string(),
    };
    assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
}

#[test]
fn stream_error_server_error_classifies_retryable_with_status() {
    let err = ProviderError::StreamError {
        reason: "HTTP 503 Service Unavailable: overloaded".to_string(),
        transient: Some(TransientKind::ServerError { status: 503 }),
    };
    assert_eq!(
        err.class(),
        retryable(TransientKind::ServerError { status: 503 })
    );
}

#[test]
fn stream_error_timeout_classifies_retryable_timeout() {
    let err = ProviderError::StreamError {
        reason: "SSE stream timed out: no data received for 30.0s".to_string(),
        transient: Some(TransientKind::Timeout),
    };
    assert_eq!(err.class(), retryable(TransientKind::Timeout));
}

#[test]
fn stream_error_without_transient_classifies_terminal() {
    let err = ProviderError::StreamError {
        reason: "provider stream ended without a Done event".to_string(),
        transient: None,
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
    assert!(!err.is_retryable());
}

/// The reason text carries zero classification weight: the old
/// magic-string triggers (`HTTP 5` prefix, `timed out` substring)
/// embedded in the reason no longer opt a terminal stream error into
/// retry.
#[test]
fn stream_error_classifies_by_structured_data_not_reason_text() {
    for reason in ["HTTP 503: service unavailable", "read timed out"] {
        let err = ProviderError::StreamError {
            reason: reason.to_string(),
            transient: None,
        };
        assert_eq!(err.class(), ErrorClass::Terminal, "{reason}");
    }
}

#[test]
fn rate_limited_classifies_rate_limited_with_delay_hint() {
    let err = ProviderError::RateLimited {
        retry_after: Some(Duration::from_secs(30)),
    };
    let class = err.class();
    assert_eq!(
        class,
        ErrorClass::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        }
    );
    assert!(class.is_retryable());
    assert_eq!(class.retry_after(), Some(Duration::from_secs(30)));
}

#[test]
fn rate_limited_without_delay_still_classifies_rate_limited() {
    let err = ProviderError::RateLimited { retry_after: None };
    assert_eq!(err.class(), ErrorClass::RateLimited { retry_after: None });
    assert_eq!(err.class().retry_after(), None);
}

#[test]
fn authentication_failed_classifies_auth() {
    let err = ProviderError::AuthenticationFailed {
        reason: "401 Unauthorized".to_string(),
    };
    assert_eq!(err.class(), ErrorClass::Auth);
    assert!(!err.is_retryable());
}

#[test]
fn oauth_credential_failures_preserve_kind_and_classify_auth() {
    for kind in [
        OAuthCredentialFailureKind::Permanent,
        OAuthCredentialFailureKind::Undurable,
        OAuthCredentialFailureKind::Conflict,
        OAuthCredentialFailureKind::Indeterminate,
    ] {
        let err = ProviderError::OAuthCredentialFailure {
            kind,
            reason: "redacted lifecycle failure".to_owned(),
        };
        assert_eq!(err.class(), ErrorClass::Auth);
        assert!(!err.is_retryable());
        assert!(err.to_string().contains(&kind.to_string()));
    }
}

#[test]
fn redirect_policy_refusal_classifies_terminal() {
    let err = ProviderError::RedirectPolicyRefused {
        status: 307,
        backend: "responses",
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
    assert!(!err.is_retryable());
    assert!(!err.to_string().contains("Location"));
}

#[test]
fn response_parse_error_classifies_terminal() {
    let err = ProviderError::ResponseParseError {
        reason: "unexpected JSON shape".to_string(),
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
}

#[test]
fn request_serialization_failed_classifies_terminal() {
    let err = ProviderError::RequestSerializationFailed {
        reason: "failed to serialize responses request: key must be a string".to_string(),
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
    assert!(!err.is_retryable());
}

#[test]
fn provider_state_identity_failures_are_payload_free_and_terminal() {
    for error in [
        ProviderError::ProviderStateIdentityRequired,
        ProviderError::ProviderStateIdentityMismatch,
        ProviderError::ProviderStateReplayUnavailable,
        ProviderError::ProviderStateProvenanceInvalid,
    ] {
        assert_eq!(error.class(), ErrorClass::Terminal);
        assert!(!error.is_retryable());
        let rendered = format!("{error:?}");
        assert!(!rendered.contains("account"));
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("digest"));
    }
}

#[test]
fn unsupported_feature_classifies_terminal() {
    let err = ProviderError::UnsupportedFeature {
        feature: "custom_grammar".to_string(),
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
}

#[test]
fn context_window_exceeded_classifies_terminal() {
    assert_eq!(
        ProviderError::ContextWindowExceeded.class(),
        ErrorClass::Terminal
    );
}

#[test]
fn quota_exceeded_classifies_terminal() {
    assert_eq!(ProviderError::QuotaExceeded.class(), ErrorClass::Terminal);
}

#[test]
fn invalid_request_classifies_terminal() {
    let err = ProviderError::InvalidRequest {
        message: "bad prompt".to_string(),
    };
    assert_eq!(err.class(), ErrorClass::Terminal);
}

// -- NornError: provider delegation, everything else terminal -----------

#[test]
fn norn_error_delegates_provider_classification() {
    let err = NornError::Provider(ProviderError::StreamInterrupted {
        reason: "reset".to_string(),
    });
    assert_eq!(err.class(), retryable(TransientKind::ConnectionReset));
    assert!(err.is_retryable());
}

#[test]
fn norn_error_non_provider_variants_classify_terminal() {
    let cases: Vec<NornError> = vec![
        NornError::Schema(Box::new(SchemaError::InvalidSchema {
            reason: "not an object".to_string(),
        })),
        NornError::Tool(ToolError::ToolNotFound {
            name: "missing".to_string(),
        }),
        NornError::Rules(RulesError::ParseFailed {
            reason: "bad rule".to_string(),
        }),
        NornError::Agent(AgentError::NotFound {
            path: "/x".to_string(),
        }),
        NornError::Session(SessionError::StorageError {
            reason: "disk gone".to_string(),
        }),
        NornError::Integration(IntegrationError::HookError {
            reason: "hook failed".to_string(),
        }),
        NornError::Config(ConfigError::MissingField {
            field: "model".to_string(),
        }),
        NornError::Skill(SkillError::MissingDescription),
        NornError::HookBlocked {
            hook_type: HookType::PreTool,
            reason: "policy".to_string(),
        },
    ];
    for err in cases {
        assert_eq!(err.class(), ErrorClass::Terminal, "{err}");
        assert!(!err.is_retryable(), "{err}");
    }
}

// -- ErrorClass: serialization round-trips across boundaries ------------

#[test]
fn error_class_serde_round_trips_every_shape() -> Result<(), serde_json::Error> {
    let cases = vec![
        ErrorClass::Retryable {
            kind: TransientKind::Timeout,
        },
        ErrorClass::Retryable {
            kind: TransientKind::ConnectionReset,
        },
        ErrorClass::Retryable {
            kind: TransientKind::ServerError { status: 502 },
        },
        ErrorClass::RateLimited {
            retry_after: Some(Duration::from_millis(1500)),
        },
        ErrorClass::RateLimited { retry_after: None },
        ErrorClass::Auth,
        ErrorClass::Terminal,
    ];
    for class in cases {
        let json = serde_json::to_string(&class)?;
        let back: ErrorClass = serde_json::from_str(&json)?;
        assert_eq!(back, class, "round trip failed for {json}");
    }
    Ok(())
}

#[test]
fn error_class_serializes_with_stable_tag() -> Result<(), serde_json::Error> {
    let json = serde_json::to_value(ErrorClass::Retryable {
        kind: TransientKind::ServerError { status: 503 },
    })?;
    assert_eq!(json["class"], "retryable");
    assert_eq!(json["kind"]["kind"], "server_error");
    assert_eq!(json["kind"]["status"], 503);

    let json = serde_json::to_value(ErrorClass::Terminal)?;
    assert_eq!(json["class"], "terminal");
    Ok(())
}
