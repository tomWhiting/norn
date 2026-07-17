#[test]
fn top_level_fork_owns_copied_response_audio_after_source_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let manager = SessionManager::new(directory.path());
    let source = manager.create_with_id(
        "audio-fork-source",
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let audio_store = source
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("source audio store is missing"))?;
    let mut writer = audio_store.begin(1)?;
    let raw = crate::provider::openai::response_stream_event::ResponseStreamEvent::from_raw(
        serde_json::json!({
            "type": "response.audio.delta",
            "sequence_number": 1,
            "delta": "Zm9yayBhdWRpbw==",
        }),
    )?;
    let event = crate::provider::response_audio::ResponseAudioEvent::from_stream_event(&raw)?
        .ok_or_else(|| std::io::Error::other("audio fixture was not audio"))?;
    writer.append(&raw, &event)?;
    let reference = writer.seal(Some("resp_fork_audio"))?;
    let link_base = EventBase::new(None);
    let assistant_base = EventBase::new(Some(link_base.id.clone()));
    let link = crate::session::ResponseAudioArtifactLink::new(
        assistant_base.id.clone(),
        reference,
        Some("resp_fork_audio".to_owned()),
    )
    .into_custom_event(link_base)?;
    source.store.append(link)?;
    source.store.append(SessionEvent::AssistantMessage {
        base: assistant_base,
        response_items: Vec::new(),
        content: "audio response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_fork_audio".to_owned()),
    })?;
    source.store.checkpoint()?;
    let source_id = source.entry.id.clone();
    drop(source);

    let fork = manager.fork(
        &source_id,
        options("model"),
        DurabilityPolicy::Flush,
    )?;
    let fork_entry = fork.entry.clone();
    drop(fork);
    manager.delete(&source_id)?;

    let resumed = manager.resume(&fork_entry.id, DurabilityPolicy::Flush)?;
    let copied = resumed
        .store
        .response_audio()
        .ok_or_else(|| std::io::Error::other("fork audio store is missing"))?
        .read(reference)?;
    assert_eq!(copied.audio, b"fork audio");
    assert!(!directory.path().join(source_id).exists());
    Ok(())
}
