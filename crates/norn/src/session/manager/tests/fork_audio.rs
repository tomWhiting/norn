fn append_top_level_fork_audio(
    store: &crate::session::store::EventStore,
    attempt: u32,
    delta: &str,
    response_id: &str,
) -> Result<crate::session::ResponseAudioArtifactRef, Box<dyn std::error::Error>> {
    let audio_store = store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("source audio store is missing"))?;
    let mut writer = audio_store.begin(attempt)?;
    let raw = crate::provider::openai::response_stream_event::ResponseStreamEvent::from_raw(
        serde_json::json!({
            "type": "response.audio.delta",
            "sequence_number": 1,
            "delta": delta,
        }),
    )?;
    let event = crate::provider::response_audio::ResponseAudioEvent::from_stream_event(&raw)?
        .ok_or_else(|| std::io::Error::other("audio fixture was not audio"))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(Some(response_id))?;

    let link_base = EventBase::new(store.last_event_id());
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let link = crate::session::ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some(response_id.to_owned()),
    )
    .into_custom_event(link_base)?;
    store.append(link)?;
    store.append(SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: format!("audio response {attempt}"),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some(response_id.to_owned()),
    })?;
    Ok(reference)
}

#[test]
fn top_level_fork_resolves_two_audio_artifacts_after_source_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let manager = SessionManager::new(directory.path());
    let source = manager.create_with_id(
        "audio-fork-source",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let first_reference = append_top_level_fork_audio(
        &source.store,
        1,
        "Zmlyc3QgZm9yayBhdWRpbw==",
        "resp_fork_audio_first",
    )?;
    let second_reference = append_top_level_fork_audio(
        &source.store,
        2,
        "c2Vjb25kIGZvcmsgYXVkaW8=",
        "resp_fork_audio_second",
    )?;
    assert_ne!(first_reference, second_reference);
    source.store.checkpoint()?;
    let source_id = source.entry.id.clone();
    drop(source);

    let fork = manager.fork(&source_id, options("model"), DurabilityPolicy::Flush)?;
    let fork_entry = fork.entry.clone();
    drop(fork);
    manager.delete(&source_id)?;

    let resumed = manager.resume(&fork_entry.id, DurabilityPolicy::Flush)?;
    let links = crate::session::response_audio_artifact_links(&resumed.store.events())?;
    assert_eq!(links.len(), 2);
    let audio_store = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("fork audio store is missing"))?;
    let listed = audio_store.list()?;
    assert_eq!(listed.len(), 2);
    assert!(listed.contains(&first_reference));
    assert!(listed.contains(&second_reference));

    for (reference, expected_audio) in [
        (first_reference, b"first fork audio".as_slice()),
        (second_reference, b"second fork audio".as_slice()),
    ] {
        let link = links
            .iter()
            .find(|candidate| candidate.reference() == reference)
            .ok_or_else(|| std::io::Error::other("fork audio link is missing"))?;
        assert_eq!(audio_store.read_linked(link)?.audio, expected_audio);
    }
    assert!(!directory.path().join(source_id).exists());
    Ok(())
}
