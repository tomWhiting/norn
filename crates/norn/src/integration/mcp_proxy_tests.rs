use super::qualified_tool_name;

#[test]
fn provider_names_are_safe_bounded_and_pair_qualified() {
    let first = qualified_tool_name("a", "b__c");
    let second = qualified_tool_name("a__b", "c");
    let punctuation = qualified_tool_name("docs", "read.file/with spaces");

    assert_ne!(first, second);
    assert!(punctuation.len() <= 64);
    assert!(
        punctuation
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    );
}
