use std::error::Error;

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn stdio_definition_is_complete_and_deterministic() -> TestResult {
    let definition = build_definition(
        Some("server".to_owned()),
        vec!["--mode".to_owned(), "read".to_owned()],
        None,
        vec!["B=2".to_owned(), "A=1".to_owned()],
        Vec::new(),
    )?;

    assert_eq!(definition.transport.as_deref(), Some("stdio"));
    assert_eq!(definition.command.as_deref(), Some("server"));
    assert_eq!(
        definition.args.as_deref(),
        Some(&["--mode".to_owned(), "read".to_owned()][..])
    );
    let keys: Vec<_> = definition
        .env
        .as_ref()
        .into_iter()
        .flat_map(|entries| entries.keys().map(String::as_str))
        .collect();
    assert_eq!(keys, ["A", "B"]);
    assert!(definition.url.is_none());
    assert!(definition.headers.is_none());
    Ok(())
}

#[test]
fn http_definition_keeps_headers_out_of_debug_output() -> TestResult {
    let definition = build_definition(
        None,
        Vec::new(),
        Some("https://example.test/private/path".to_owned()),
        Vec::new(),
        vec!["Authorization=Bearer sentinel-secret".to_owned()],
    )?;

    assert_eq!(definition.transport.as_deref(), Some("http"));
    assert_eq!(
        definition.url.as_deref(),
        Some("https://example.test/private/path")
    );
    let rendered = format!("{definition:?}");
    assert!(!rendered.contains("sentinel-secret"));
    assert!(!rendered.contains("private/path"));
    Ok(())
}

#[test]
fn duplicate_entry_key_is_rejected_without_disclosing_values() -> TestResult {
    let result = parse_entries(
        "header",
        vec![
            "Authorization=first-secret".to_owned(),
            "Authorization=second-secret".to_owned(),
        ],
    );
    let Err(error) = result else {
        return Err("duplicate key must be rejected".into());
    };
    let rendered = error.to_string();
    assert!(rendered.contains("Authorization"));
    assert!(!rendered.contains("first-secret"));
    assert!(!rendered.contains("second-secret"));
    Ok(())
}
