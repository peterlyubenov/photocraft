use super::*;
#[test]
fn form_can_be_driven_and_inspected_without_submitting_jobs() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let ctx = egui::Context::default();
    let (request, _) = crate::control::ControlRequest::new("ui.set", json!({"ai":{"open":true,"request":{"prompt":"synthetic test"},"countChoice":2}}));
    let crate::control::Outcome::Done(reply) = crate::control::handle(&mut app, &ctx, &request) else { panic!("unexpected asynchronous form patch") };
    assert_eq!(reply["ok"], true);
    assert_eq!(app.ui.ai.request.width, 512);
    assert_eq!(app.ui.ai.request.prompt, "synthetic test");
    assert_eq!(crate::control::inspect(&app, &ctx)["ai"]["countChoice"], 2);
    assert!(!app.session.ai.status.running);
    let before = app.ui.ai.clone();
    for patch in [json!({"request":{"promtp":"typo"}}), json!({"customCount":17}), json!({"token":"secret"}), json!({"settings":{"contextPadding":2049}})] {
        assert!(patch_form(&before, &patch).is_err());
    }
    assert_eq!(app.ui.ai, before);
}
#[test]
fn panel_state_uses_existing_menu_and_does_not_persist_secrets() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    menu(&mut app, &json!({"show":true})).unwrap();
    assert!(app.ui.ai.open);
    assert_eq!(app.ui.ai.count_choice, 1);
    assert!(app.ui.ai.random_seed);
    app.ui.ai.token = "do-not-save".into();
    let serialized = serde_json::to_string(&app.ui).unwrap();
    assert!(!serialized.contains("do-not-save"));
    assert_eq!(crate::menus::menu_items(&app).iter().find(|item| item.id == "window.aiGeneration").unwrap().checked, Some(true));
    assert!(crate::menus::menu_items(&app).iter().any(|item| item.id == "window.aiGeneration"));
    assert_eq!(app.session.documents().len(), 0);
}
#[test]
fn panel_renders_with_no_backend_or_workflow() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.ui.ai.open = true;
    let mut harness = egui_kittest::Harness::new_ui_state(
        |ui, app: &mut PhotocraftApp| {
            window(app, ui.ctx());
        },
        app,
    );
    harness.run();
    assert!(harness.state().ui.ai.open);
    assert!(!harness.state().session.ai.status.running);
}

#[test]
fn visible_accept_and_discard_buttons_dispatch_document_commands() {
    use egui_kittest::kittest::Queryable;
    use photocraft_engine::ai::{images::prepare, queue::Candidate};
    let workflow: Workflow = serde_json::from_value(json!({
        "name":"UI test", "graph":{"id":"test","nodes":{"out":{"id":"out","type":"mock","prompt":"","width":8,"height":8}},"edges":[]},
        "bindings":{"prompt":["/nodes/out/prompt"],"width":["/nodes/out/width"],"height":["/nodes/out/height"]},
        "outputNode":"out","modes":["generate"],"dimensionMultiple":1
    }))
    .unwrap();
    for action in ["Accept as layer", "Discard"] {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.add_document(
            photocraft_doc::Document::new("test", photocraft_doc::Size::new(32, 32), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U16),
            None,
        );
        let st = app.session.active().unwrap();
        let placement =
            prepare(&st.doc, st.revision, st.active_layer, &Request { prompt: "test".into(), width: 8, height: 8, ..Default::default() }, &workflow, 0)
                .unwrap()
                .placement;
        app.session.ai.candidates.push(Candidate {
            id: 1,
            placement,
            image: photocraft_codecs::Image::from_u8(8, 8, photocraft_codecs::ChannelLayout::Rgb, vec![255; 8 * 8 * 3]).unwrap(),
            metadata: json!({"seed":42}),
        });
        let mut h = egui_kittest::Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                window(app, ui.ctx());
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::default());
        h.state_mut().ui.ai.open = true;
        h.run();
        let point = h.get_by_label(action).rect().center();
        assert!(h.ctx.content_rect().contains(point));
        h.event(egui::Event::PointerMoved(point));
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.run();
        assert!(h.state().session.ai.candidates.is_empty());
        assert_eq!(h.state().session.active().unwrap().doc.layers.len(), usize::from(action == "Accept as layer"));
        if action == "Accept as layer" {
            assert!(h.state_mut().session.undo());
            assert_eq!(h.state().session.active().unwrap().doc.layers.len(), 0);
        }
    }
}
