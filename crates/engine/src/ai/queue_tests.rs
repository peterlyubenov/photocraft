#![allow(clippy::unwrap_used)] // Synthetic test setup helpers.
use super::queue::generate;
use super::*;
use photocraft_codecs::{ChannelLayout, Image};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};

struct Mock {
    submitted: u32,
    cancelled: u32,
    fail: bool,
    bytes: Vec<u8>,
}
impl Mock {
    fn new() -> Self {
        Self {
            submitted: 0,
            cancelled: 0,
            fail: false,
            bytes: images::encode_png(&Image::from_u8(8, 8, ChannelLayout::Rgb, vec![127; 8 * 8 * 3]).unwrap()).unwrap(),
        }
    }
}
impl Backend for Mock {
    fn health(&mut self) -> AiResult<String> {
        Ok("mock".into())
    }
    fn validate_graph(&self, w: &workflow::Workflow) -> AiResult<()> {
        w.validate()
    }
    fn upload(&mut self, _: Vec<u8>, _: bool) -> AiResult<String> {
        Ok("ref.png".into())
    }
    fn submit(&mut self, v: Value) -> AiResult<u64> {
        assert_eq!(v.pointer("/nodes/out/prompt"), Some(&json!("test")));
        self.submitted += 1;
        Ok(7)
    }
    fn poll(&mut self, _: u64, _: &str) -> AiResult<JobStatus> {
        if self.fail { Err(AiError::Backend("disconnected".into())) } else { Ok(JobStatus::Complete("out.png".into())) }
    }
    fn cancel(&mut self, _: u64) -> AiResult<()> {
        self.cancelled += 1;
        Ok(())
    }
    fn image(&mut self, _: &str) -> AiResult<Vec<u8>> {
        Ok(self.bytes.clone())
    }
}
fn prepared(r: &Request, w: &workflow::Workflow) -> images::Prepared {
    let d = photocraft_doc::Document::new("test", photocraft_doc::Size::new(32, 32), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
    images::prepare(&d, 1, None, r, w, 0).unwrap()
}
#[test]
fn completion_cancellation_timeout_and_recovery() {
    let w = crate::ai_cmds::tests::workflow();
    let r = Request { prompt: "test".into(), width: 8, height: 8, ..Default::default() };
    let flag = AtomicBool::new(false);
    let mut m = Mock::new();
    let c = generate(&mut m, prepared(&r, &w), &w, &r, 1, 5, &flag, |_, _| {}).unwrap();
    assert_eq!(c.id, 1);
    assert_eq!(m.submitted, 1);
    assert_eq!(m.cancelled, 0);
    flag.store(true, Ordering::Relaxed);
    assert!(matches!(generate(&mut m, prepared(&r, &w), &w, &r, 2, 5, &flag, |_, _| {}), Err(AiError::Cancelled)));
    assert_eq!(m.submitted, 1);
    flag.store(false, Ordering::Relaxed);
    assert!(matches!(generate(&mut m, prepared(&r, &w), &w, &r, 2, 0, &flag, |_, _| {}), Err(AiError::Timeout)));
    assert_eq!(m.cancelled, 1);
    m.fail = true;
    assert!(generate(&mut m, prepared(&r, &w), &w, &r, 2, 5, &flag, |_, _| {}).is_err());
    assert_eq!(m.cancelled, 2);
    m.fail = false;
    assert!(generate(&mut m, prepared(&r, &w), &w, &r, 3, 5, &flag, |_, _| {}).is_ok());
    assert!(matches!(
        generate(&mut m, prepared(&r, &w), &w, &r, 4, 5, &flag, |message, _| {
            if message == "Downloading result" {
                flag.store(true, Ordering::Relaxed);
            }
        }),
        Err(AiError::Cancelled)
    ));
}

#[test]
fn inpainting_submits_both_image_fields_and_keeps_original_mask() {
    let mut w = crate::ai_cmds::tests::workflow();
    w.modes.push(Mode::Inpaint);
    for key in ["reference", "mask"] {
        w.graph["nodes"]["out"][key] = json!({"image_name":"placeholder.png"});
        w.bindings.insert(key.into(), vec![format!("/nodes/out/{key}")]);
    }
    w.mask_semantics = Some(workflow::MaskSemantics::WhiteRepaints);
    let mut d = photocraft_doc::Document::with_background(
        "test",
        photocraft_doc::Size::new(32, 32),
        photocraft_color::ColorMode::Rgb,
        photocraft_color::SampleType::U8,
        photocraft_color::Color::rgb(0.5, 0.5, 0.5),
    );
    let mut mask = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
    mask.fill_rect(photocraft_geom::Rect::from_xywh(10, 10, 8, 8), &[1.0]);
    d.selection = Some(mask);
    let r = Request { mode: Mode::Inpaint, prompt: "test".into(), ..Default::default() };
    let prepared = images::prepare(&d, 1, d.top_layer(), &r, &w, 0).unwrap();
    let original = prepared.placement.mask.clone();
    assert!(prepared.mask.is_some());
    assert!(prepared.reference.is_some());
    let mut mock = Mock::new();
    let flag = AtomicBool::new(false);
    let result = generate(&mut mock, prepared, &w, &r, 1, 5, &flag, |_, _| {}).unwrap();
    assert_eq!(result.metadata["parameters"]["reference"]["image_name"], "ref.png");
    assert_eq!(result.metadata["parameters"]["mask"]["image_name"], "ref.png");
    assert_eq!(result.placement.mask, original);
}
