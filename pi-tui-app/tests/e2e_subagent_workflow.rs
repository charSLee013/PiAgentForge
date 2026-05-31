mod common;

#[test]
fn e2e_subagent_workflow_respects_busy_and_persists_outputs() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    common::setup_provider(&rt, 50);

    let dir = common::unique_dir("pi_e2e_subagent_workflow");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("subagent.jsonl");
    let model = common::delayed_model();
    let mut im = rt
        .block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
        .unwrap();

    im.set_editor_text_for_test("first");
    rt.block_on(im.send_message_for_test());
    assert!(im.is_streaming_for_test());

    let err = rt
        .block_on(im.run_subagent_command_for_test(Some("single inspect repo")))
        .expect_err("subagent should reject while streaming");
    assert!(err.to_string().contains("while a run is active"));
    common::spin_until_idle(&rt, &mut im);

    im.set_editor_text_for_test("/subagent single inspect repo layout");
    rt.block_on(im.send_message_for_test());
    let text = im.latest_assistant_text_for_test().unwrap_or_default();
    assert!(text.contains("Subagent (single)"), "{text:?}");

    im.set_editor_text_for_test("/subagent parallel inspect repo || inspect tests");
    rt.block_on(im.send_message_for_test());
    let text = im.latest_assistant_text_for_test().unwrap_or_default();
    assert!(text.contains("Subagent (parallel)"), "{text:?}");

    im.set_editor_text_for_test("/subagent chain inspect repo || summarize {previous}");
    rt.block_on(im.send_message_for_test());
    let text = im.latest_assistant_text_for_test().unwrap_or_default();
    assert!(text.contains("Subagent (chain)"), "{text:?}");
    assert!(!text.contains("{previous}"), "{text:?}");

    let resumed =
        rt.block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
            .unwrap();
    let texts = common::collect_texts(resumed.session_for_test().build_context().messages);
    assert!(texts.iter().any(|text| text.contains("Subagent (single)")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("Subagent (parallel)")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("Subagent (chain)")), "{texts:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
