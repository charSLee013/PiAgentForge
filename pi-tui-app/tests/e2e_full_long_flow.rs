mod common;

#[test]
fn e2e_full_long_flow_smoke() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    common::setup_provider(&rt, 30);

    let dir = common::unique_dir("pi_e2e_full_long_flow");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("full.jsonl");
    let model = common::delayed_model();

    let mut im = rt
        .block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
        .unwrap();

    im.set_editor_text_for_test("first");
    rt.block_on(im.send_message_for_test());
    im.set_editor_text_for_test("/steer steer next");
    rt.block_on(im.send_message_for_test());
    im.set_editor_text_for_test("/follow-up follow later");
    rt.block_on(im.send_message_for_test());
    common::spin_until_idle(&rt, &mut im);

    im.set_editor_text_for_test("/subagent single inspect repo layout");
    rt.block_on(im.send_message_for_test());

    for i in 0..6 {
        im.set_editor_text_for_test(&format!("message {i} {}", "x".repeat(400)));
        rt.block_on(im.send_message_for_test());
        common::spin_until_idle(&rt, &mut im);
    }

    im.set_editor_text_for_test("/compact Keep the summary terse");
    rt.block_on(im.send_message_for_test());
    im.set_editor_text_for_test("/export");
    rt.block_on(im.send_message_for_test());

    let resumed =
        rt.block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
            .unwrap();
    let texts = common::collect_texts(resumed.session_for_test().build_context().messages);
    assert!(
        texts.iter().any(|text| text.contains("echo:Summarize the following conversation context."))
            || texts.iter().any(|text| text.contains("Subagent (single)")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|text| text.contains("message 4")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("message 5")), "{texts:?}");
    assert!(path.with_extension("html").exists());

    let _ = std::fs::remove_dir_all(&dir);
}
