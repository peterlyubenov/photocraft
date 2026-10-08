//! Opt-in expensive integration test. Normal tests never invoke a model.
use super::*;
use serde_json::json;
use std::sync::atomic::AtomicBool;
#[test]
#[ignore = "expensive real generation; requires explicit opt-in flag and a tested template"]
fn live_generation_smoke() {
    assert_eq!(std::env::var("PHOTOCRAFT_AI_SMOKE_GENERATE").as_deref(), Ok("1"), "set PHOTOCRAFT_AI_SMOKE_GENERATE=1 to authorize one real generation");
    let path = std::env::var("PHOTOCRAFT_AI_SMOKE_WORKFLOW").expect("set PHOTOCRAFT_AI_SMOKE_WORKFLOW to a tested PhotoCraft template JSON");
    let workflow: workflow::Workflow = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    workflow.validate().unwrap();
    let mode: Mode = serde_json::from_value(json!(std::env::var("PHOTOCRAFT_AI_SMOKE_MODE").unwrap_or_else(|_| "generate".into()))).unwrap();
    let mut document = photocraft_doc::Document::with_background(
        "AI Smoke",
        photocraft_doc::Size::new(512, 512),
        photocraft_color::ColorMode::Rgb,
        photocraft_color::SampleType::U8,
        photocraft_color::Color::rgb(0.4, 0.5, 0.6),
    );
    if matches!(mode, Mode::MasklessFill | Mode::Inpaint) {
        let mut mask = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
        mask.fill_rect(photocraft_geom::Rect::from_xywh(160, 160, 192, 192), &[1.0]);
        document.selection = Some(mask);
    }
    let settings = Settings { server_url: std::env::var("PHOTOCRAFT_INVOKE_URL").unwrap_or_else(|_| Settings::default().server_url), ..Default::default() };
    let request = Request {
        prompt: std::env::var("PHOTOCRAFT_AI_SMOKE_PROMPT").unwrap_or_else(|_| "A small green cube on a neutral background".into()),
        mode,
        width: 512,
        height: 512,
        count: Some(1),
        ..Default::default()
    };
    let prepared = images::prepare(&document, 1, document.top_layer(), &request, &workflow, 32).unwrap();
    let mut backend = invoke::InvokeClient::new(&settings, std::env::var("PHOTOCRAFT_INVOKE_TOKEN").unwrap_or_default()).unwrap();
    println!("{}", backend.health().unwrap());
    let candidate =
        queue::generate(&mut backend, prepared, &workflow, &request, 1, settings.job_timeout_secs, &AtomicBool::new(false), |message, _| println!("{message}"))
            .unwrap();
    let layer = images::result_layer(&document, &candidate.placement, &candidate.image, candidate.metadata.clone(), true).unwrap();
    assert_eq!(layer.surface().unwrap().format(), document.pixel_format());
    assert_eq!(layer.mask.is_some(), document.selection.is_some());
    assert_eq!(document.layers.len(), 1);
}
