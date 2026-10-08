//! Offscreen result-panel review with synthetic pixels. This never connects to InvokeAI.
//! cargo run -p photocraft-ui-egui --example ai_snapshot -- /tmp/ai-results.png [--masked] [--light] [--small]
use photocraft_codecs::{ChannelLayout, Image};
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Size};
use photocraft_engine::{
    Session,
    ai::{Mode, Request, Settings, images::prepare, queue::Candidate, workflow::Workflow},
};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let out = args.get(1).ok_or("supply the destination PNG path")?;
    let workflow: Workflow = serde_json::from_value(json!({
        "name":"Synthetic preview fixture (no inference)",
        "graph":{"id":"fixture","nodes":{"out":{"id":"out","type":"mock","prompt":"","width":512,"height":512,"seed":1,"reference":{"image_name":"fixture.png"}}},"edges":[]},
        "bindings":{"prompt":["/nodes/out/prompt"],"width":["/nodes/out/width"],"height":["/nodes/out/height"],"seed":["/nodes/out/seed"],"reference":["/nodes/out/reference"]},
        "outputNode":"out","modes":["masklessFill"],"dimensionMultiple":8
    }))?;
    let mut doc = Document::with_background("Synthetic preview", Size::new(1024, 768), ColorMode::Rgb, SampleType::U16, Color::rgb(0.15, 0.25, 0.45));
    let mut mask = Surface::new(PixelFormat { sample: SampleType::F32, ..PixelFormat::GRAY8 });
    mask.fill_rect(Rect::from_xywh(360, 220, 240, 240), &[0.35]);
    mask.fill_rect(Rect::from_xywh(384, 244, 192, 192), &[1.0]);
    doc.selection = Some(mask);
    let request = Request { prompt: "Add a green geometric tile. Synthetic preview only.".into(), mode: Mode::MasklessFill, ..Default::default() };
    let mut session = Session::new();
    session.add_document(doc, None);
    let st = session.active().ok_or("missing synthetic document")?;
    let prepared = prepare(&st.doc, st.revision, st.active_layer, &request, &workflow, 32)?;
    let mut pixels = Vec::with_capacity(256 * 256 * 3);
    for y in 0..256 {
        for x in 0..256 {
            pixels.extend(if (64..192).contains(&x) && (64..192).contains(&y) { [48, 180, 96] } else { [72, 110, 170] });
        }
    }
    session.ai.candidates.push(Candidate {
        id: 1,
        placement: prepared.placement,
        image: Image::from_u8(256, 256, ChannelLayout::Rgb, pixels)?,
        metadata: json!({"seed":42}),
    });
    session.ai.status.message = "Completed 1 / 1 — awaiting acceptance".into();
    session.ai.status.completed = 1;
    session.ai.status.total = 1;
    let settings = Settings { workflows: vec![workflow], ..Default::default() };
    session.execute("ai.configure", json!({"settings":settings}))?;
    if args.iter().any(|arg| arg == "--running") {
        session.ai.status.running = true;
        session.ai.status.total = 4;
        session.ai.status.message = "2/4: Generating — 4/24 graph nodes completed (synthetic fixture)".into();
    }
    let mut app = PhotocraftApp::new(session, Services::default());
    app.ui.ai.open = true;
    if args.iter().any(|arg| arg == "--running") {
        app.ui.ai.count_choice = 4;
    }
    app.ui.ai.request = request;
    app.ui.ai.request.width = 512;
    app.ui.ai.request.height = 512;
    app.ui.ai.preview_mask = args.iter().any(|arg| arg == "--masked");
    if args.iter().any(|arg| arg == "--light") {
        app.ui.theme = photocraft_ui_egui::theme::ThemeKind::StudioLight;
    }
    let size = if args.iter().any(|arg| arg == "--small") { egui::vec2(800.0, 600.0) } else { egui::vec2(1440.0, 900.0) };
    let mut harness = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(1.0).wgpu().build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, app.ui.theme);
        app
    });
    if args.iter().any(|arg| arg == "--light") {
        let ctx = harness.ctx.clone();
        harness.state_mut().set_theme(&ctx, photocraft_ui_egui::theme::ThemeKind::StudioLight);
    }
    harness.run_steps(16);
    harness.render()?.save(out)?;
    println!("wrote {out}; no network or generation requests were made");
    Ok(())
}
