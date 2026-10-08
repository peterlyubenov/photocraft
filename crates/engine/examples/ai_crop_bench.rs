//! Release measurement of AI reference preparation on a synthetic 24 MP document; no inference.
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Size};
use photocraft_engine::ai::{Mode, Request, images::prepare, workflow::Workflow};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::json;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workflow: Workflow = serde_json::from_value(json!({
        "name":"Synthetic benchmark (no inference)",
        "graph":{"id":"benchmark","nodes":{"out":{"id":"out","type":"mock","prompt":"","width":1024,"height":1024,"reference":{"image_name":"fixture.png"}}},"edges":[]},
        "bindings":{"prompt":["/nodes/out/prompt"],"width":["/nodes/out/width"],"height":["/nodes/out/height"],"reference":["/nodes/out/reference"]},
        "outputNode":"out","modes":["edit"],"maxWidth":1024,"maxHeight":1024,"dimensionMultiple":8
    }))?;
    let mut doc = Document::with_background("AI benchmark", Size::new(6000, 4000), ColorMode::Rgb, SampleType::U8, Color::rgb(0.2, 0.4, 0.6));
    let request = Request { mode: Mode::Edit, prompt: "benchmark only".into(), ..Default::default() };
    for selected in [false, true] {
        if selected {
            let mut selection = Surface::new(PixelFormat::GRAY8);
            selection.fill_rect(Rect::from_xywh(2700, 1700, 512, 512), &[1.0]);
            doc.selection = Some(selection);
        }
        let start = Instant::now();
        let result = prepare(&doc, 1, doc.top_layer(), &request, &workflow, 32)?;
        println!(
            "{}: {:.1} ms, document crop {}×{}, model {}×{}, PNG {} bytes",
            if selected { "selection + 32 px context" } else { "full 24 MP reference" },
            start.elapsed().as_secs_f64() * 1000.0,
            result.placement.rect.width(),
            result.placement.rect.height(),
            result.width,
            result.height,
            result.reference.as_ref().map_or(0, Vec::len)
        );
    }
    Ok(())
}
