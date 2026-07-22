use super::*;

#[test]
fn register_slash_commands_registers_user_invocable_skill() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "deploy", "deploy the service")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let mut registry = SlashCommandRegistry::new();
    catalog.register_slash_commands(&mut registry);

    let command = registry.get("deploy").ok_or("command was not registered")?;
    assert_eq!(command.name, "deploy");
    let SlashCommandHandler::Skill { skill_name } = &command.handler else {
        return Err("expected Skill handler variant".into());
    };
    assert_eq!(skill_name, "deploy");
    Ok(())
}

#[test]
fn register_slash_commands_skips_non_user_invocable() -> TestResult {
    let dir = tempdir()?;
    write_file(
        &dir.path().join("background-knowledge").join("SKILL.md"),
        "---\ndescription: background\nuser-invocable: false\n---\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let mut registry = SlashCommandRegistry::new();
    catalog.register_slash_commands(&mut registry);

    assert!(registry.get("background-knowledge").is_none());
    assert!(registry.is_empty());
    Ok(())
}

#[test]
fn register_slash_commands_preserves_existing_unrelated_entries() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "deploy", "deploy the service")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let mut registry = SlashCommandRegistry::new();
    registry.register(SlashCommand {
        name: "help".to_owned(),
        handler: SlashCommandHandler::Tool {
            tool_name: "noop".to_owned(),
            args: serde_json::json!({}),
        },
    });
    catalog.register_slash_commands(&mut registry);

    assert!(registry.get("help").is_some());
    assert!(registry.get("deploy").is_some());
    Ok(())
}

#[test]
fn preprocess_input_expands_registered_slash_skill_with_args() -> TestResult {
    use crate::r#loop::commands::{PreprocessResult, preprocess_input};

    let dir = tempdir()?;
    write_skill(dir.path(), "deploy", "deploy the service")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let mut registry = SlashCommandRegistry::new();
    catalog.register_slash_commands(&mut registry);

    let out = preprocess_input("/deploy prod", &registry)?;
    let PreprocessResult::Expanded { messages } = out else {
        return Err("expected expansion".into());
    };
    assert_eq!(messages.len(), 1);
    let body = messages
        .first()
        .and_then(|message| message.content.as_ref())
        .ok_or("skill message had no content")?;
    assert!(
        body.contains("deploy"),
        "body must mention skill name: {body}"
    );
    assert!(body.contains("prod"), "body must mention argument: {body}");
    Ok(())
}

#[test]
fn preprocess_input_expands_registered_slash_skill_without_args() -> TestResult {
    use crate::r#loop::commands::{PreprocessResult, preprocess_input};

    let dir = tempdir()?;
    write_skill(dir.path(), "deploy", "deploy the service")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let mut registry = SlashCommandRegistry::new();
    catalog.register_slash_commands(&mut registry);

    let out = preprocess_input("/deploy", &registry)?;
    let PreprocessResult::Expanded { messages } = out else {
        return Err("expected expansion".into());
    };
    let body = messages
        .first()
        .and_then(|message| message.content.as_ref())
        .ok_or("skill message had no content")?;
    assert!(body.contains("deploy"));
    assert!(
        !body.contains("Argument:"),
        "no Argument clause when slash command has no trailing text: {body}",
    );
    Ok(())
}

#[test]
fn invocation_matrix_default_is_in_listing_and_registry() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "default-skill", "default behaviour")?;
    write_file(
        &dir.path().join("model-hidden").join("SKILL.md"),
        "---\ndescription: hidden from model\ndisable-model-invocation: true\n---\n",
    )?;
    write_file(
        &dir.path().join("background").join("SKILL.md"),
        "---\ndescription: background only\nuser-invocable: false\n---\n",
    )?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let listing = catalog.system_prompt_listing();
    let mut registry = SlashCommandRegistry::new();
    catalog.register_slash_commands(&mut registry);

    // Default: in listing + registered.
    assert!(
        listing.contains("- default-skill: default behaviour"),
        "default skill must appear in listing, got: {listing}"
    );
    assert!(registry.get("default-skill").is_some());

    // disable-model-invocation: registered but not in listing.
    assert!(
        !listing.contains("model-hidden"),
        "model-hidden must not appear in listing, got: {listing}"
    );
    assert!(registry.get("model-hidden").is_some());

    // user-invocable: false — in listing but not registered.
    assert!(
        listing.contains("- background: background only"),
        "background skill must appear in listing, got: {listing}"
    );
    assert!(registry.get("background").is_none());
    Ok(())
}

#[test]
fn system_prompt_listing_has_no_trailing_newline() -> TestResult {
    let dir = tempdir()?;
    write_skill(dir.path(), "alpha", "first")?;
    write_skill(dir.path(), "beta", "second")?;

    let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);
    let listing = catalog.system_prompt_listing();
    assert!(!listing.is_empty());
    assert!(
        !listing.ends_with('\n'),
        "listing should not end with newline, ends with: {:?}",
        listing.chars().last()
    );
    Ok(())
}
