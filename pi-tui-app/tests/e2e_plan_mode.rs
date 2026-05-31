mod common;

#[test]
fn e2e_plan_mode_captures_and_executes_real_plan_output() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    common::setup_plan_provider(&rt);

    let dir = common::unique_dir("pi_e2e_plan_mode");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("plan.jsonl");
    let model = common::plan_model();

    let mut im = rt
        .block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
        .unwrap();

    im.set_editor_text_for_test("/plan");
    rt.block_on(im.send_message_for_test());
    assert!(im.plan_mode_for_test());

    im.set_editor_text_for_test("Inspect the repository and then summarize the result.");
    rt.block_on(im.send_message_for_test());
    common::spin_until_idle(&rt, &mut im);
    assert!(im.has_pending_plan_for_test(), "real plan output should be captured");

    im.set_editor_text_for_test("/plan execute");
    rt.block_on(im.send_message_for_test());
    common::spin_until_idle(&rt, &mut im);

    let resumed =
        rt.block_on(pi_tui_app::InteractiveMode::new(&model.id, model, None, None, Some(path.clone()), dir.clone()))
            .unwrap();
    let texts = common::collect_texts(resumed.session_for_test().build_context().messages);
    assert!(texts.iter().any(|text| text.contains("Plan:")), "{texts:?}");
    assert!(texts.iter().any(|text| text.contains("Execute the approved plan below.")), "{texts:?}");

    let _ = std::fs::remove_dir_all(&dir);
}
