mod common;

use pi_agent_core::session::storage;
use pi_tui_app::test_support::extract_text_from_blocks;

#[test]
fn e2e_session_workflow_persists_queue_compaction_export_and_resume() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    common::setup_provider(&rt, 50);

    let dir = common::unique_dir("pi_e2e_session_workflow");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    let model = common::delayed_model();

    let mut im = rt
        .block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
        .unwrap();

    im.set_editor_text_for_test("first");
    rt.block_on(im.send_message_for_test());
    assert!(im.is_streaming_for_test());

    im.set_editor_text_for_test("/steer steer next");
    rt.block_on(im.send_message_for_test());
    im.set_editor_text_for_test("/follow-up follow later");
    rt.block_on(im.send_message_for_test());
    common::spin_until_idle(&rt, &mut im);

    for i in 0..6 {
        im.set_editor_text_for_test(&format!("message {i} {}", "x".repeat(400)));
        rt.block_on(im.send_message_for_test());
        common::spin_until_idle(&rt, &mut im);
    }

    im.set_editor_text_for_test("/compact Keep the summary terse");
    rt.block_on(im.send_message_for_test());

    let export_path = path.with_extension("html");
    im.set_editor_text_for_test("/export");
    rt.block_on(im.send_message_for_test());

    assert!(export_path.exists(), "export should write HTML");
    let html = std::fs::read_to_string(&export_path).unwrap();
    assert!(html.contains("first"), "{html:?}");

    let (_, entries, _) = rt.block_on(storage::read_all(&path)).unwrap();
    assert!(common::has_compaction(&entries), "expected persisted compaction entry");

    let resumed = rt
        .block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
        .unwrap();
    let texts = common::collect_texts(resumed.session_for_test().build_context().messages);
    assert!(
        texts.iter().any(|text| text.contains("echo:Summarize the following conversation context."))
            || texts.iter().any(|text| text == "first"),
        "{texts:?}"
    );
    assert!(texts.iter().any(|text| text.contains("message 4")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("message 5")), "{texts:?}");
    let first_text = resumed
        .session_for_test()
        .build_context()
        .messages
        .first()
        .map(|message| extract_text_from_blocks(&message.content))
        .unwrap_or_default();
    assert!(first_text.contains("[Compaction:"), "{first_text:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
