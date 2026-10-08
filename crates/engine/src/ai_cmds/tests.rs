#![allow(clippy::unwrap_used)] // Synthetic test setup helpers.
use super::*;
use crate::ai::{
    images::{decode_result, encode_png, prepare},
    queue::Candidate,
    workflow::Workflow,
};
use photocraft_codecs::{ChannelLayout, Image};
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Size};

pub(crate) fn workflow() -> Workflow {
    serde_json::from_value(json!({"name":"Mock graph", "graph":{"id":"test", "nodes":{"out":{"id":"out","type":"mock","prompt":"","width":8,"height":8,"seed":1}},"edges":[]},"bindings":{"prompt":["/nodes/out/prompt"],"width":["/nodes/out/width"],"height":["/nodes/out/height"],"seed":["/nodes/out/seed"]}, "outputNode":"out", "modes":["generate"], "dimensionMultiple":1})).unwrap()
}
fn session(depth: SampleType) -> Session {
    let mut s = Session::new();
    s.add_document(Document::new("Test", Size::new(32, 32), ColorMode::Rgb, depth), None);
    s
}
fn candidate(s: &Session) -> Candidate {
    let st = s.active().unwrap();
    let prepared =
        prepare(&st.doc, st.revision, st.active_layer, &Request { prompt: "test".into(), width: 8, height: 8, ..Default::default() }, &workflow(), 0).unwrap();
    Candidate {
        id: 1,
        placement: prepared.placement,
        image: Image::from_u8(8, 8, ChannelLayout::Rgba, vec![255; 8 * 8 * 4]).unwrap(),
        metadata: json!({"prompt":"test"}),
    }
}
#[test]
fn graph_binding_and_validation() {
    let mut w = workflow();
    w.validate().unwrap();
    let graph = w.bind(&std::collections::BTreeMap::from([("prompt".into(), json!("literal prompt"))])).unwrap();
    assert_eq!(graph.pointer("/nodes/out/prompt"), Some(&json!("literal prompt")));
    w.bindings.insert("bad".into(), vec!["/nodes/out/type".into()]);
    assert!(w.validate().is_err());
    assert!(serde_json::from_value::<Workflow>(json!({"nodes":[]})).is_err());
    let mut w = workflow();
    w.graph["nodes"]["upstream"] = w.graph["nodes"]["out"].clone();
    w.graph["nodes"]["upstream"]["id"] = json!("upstream");
    w.graph["nodes"]["out"]["images"] = json!([{"image_name":"source.png"}]);
    w.bindings.insert("reference".into(), vec!["/nodes/out/images/0".into()]);
    w.graph["edges"] = json!([{"source":{"node_id":"upstream","field":"image"},"destination":{"node_id":"out","field":"images"}}]);
    assert!(w.validate().unwrap_err().to_string().contains("overridden"));
}
#[test]
fn insertion_undo_redo_depth_and_stale_identity() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let mut s = session(depth);
        s.ai.candidates.push(candidate(&s));
        let revision = s.active().unwrap().revision;
        assert_eq!(s.active().unwrap().doc.layers.len(), 0);
        let result = s.execute("ai.accept", json!({"id":1})).unwrap();
        let l = s.active().unwrap().doc.layer(photocraft_doc::LayerId(result["layer"].as_u64().unwrap())).unwrap();
        assert_eq!(l.surface().unwrap().format().sample, depth);
        assert!(l.surface().unwrap().sample_channel(2, 2, 0) > 0.99);
        assert_eq!(l.psd_blocks[0].0, *b"pcAI");
        assert!(s.active().unwrap().revision > revision);
        assert!(s.undo());
        assert_eq!(s.active().unwrap().doc.layers.len(), 0);
        assert!(s.redo());
    }
    let mut s = session(SampleType::U8);
    s.ai.candidates.push(candidate(&s));
    s.edit("changed", |_, _| Ok(())).unwrap();
    assert!(s.execute("ai.accept", json!({"id":1})).is_err());
    assert_eq!(s.ai.candidates.len(), 1);
    assert!(s.execute("ai.accept", json!({"id":1,"allowStale":true})).is_ok());
    s.ai.candidates.push(candidate(&s));
    s.add_document(Document::new("Other", Size::new(32, 32), ColorMode::Rgb, SampleType::U8), None);
    assert!(s.execute("ai.accept", json!({"id":1,"allowStale":true})).is_err());
    assert_eq!(s.active().unwrap().doc.layers.len(), 0);
}
#[test]
fn generated_white_round_trips_through_document_colour_modes_and_depths() {
    for mode in [ColorMode::Rgb, ColorMode::Grayscale, ColorMode::Cmyk, ColorMode::Lab] {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut s = Session::new();
            s.add_document(Document::new("Colour test", Size::new(32, 32), mode, depth), None);
            s.ai.candidates.push(candidate(&s));
            s.execute("ai.accept", json!({"id":1})).unwrap();
            let doc = &s.active().unwrap().doc;
            assert_eq!(doc.layers[0].surface().unwrap().format(), doc.pixel_format());
            let rendered = photocraft_compose::render(doc, photocraft_geom::Rect::from_xywh(0, 0, 1, 1));
            assert!(rendered.px[0].iter().all(|v| v.is_finite() && *v > 0.95), "{mode:?} {depth:?}: {:?}", rendered.px[0]);
            assert!(s.undo());
        }
    }
}
#[test]
fn settings_persist_and_discard_does_not_edit() {
    let mut s = session(SampleType::U8);
    let settings = Settings { server_url: "http://127.0.0.1:9999".into(), workflows: vec![workflow()], ..Default::default() };
    s.execute("ai.configure", json!({"settings":settings,"token":"secret"})).unwrap();
    let saved = s.prefs_to_json();
    assert!(!saved.contains("secret"));
    let mut restored = Session::new();
    restored.load_prefs_json(&saved).unwrap();
    assert_eq!(restored.prefs().ai, settings);
    s.ai.candidates.push(candidate(&s));
    let revision = s.active().unwrap().revision;
    s.execute("ai.discard", json!({"id":1})).unwrap();
    assert_eq!(s.active().unwrap().revision, revision);
}
#[test]
fn malformed_params_and_corrupted_images_are_errors() {
    let mut s = session(SampleType::U8);
    for command in ["ai.accept", "ai.discard", "ai.generate"] {
        for params in [Value::Null, json!({}), json!({"id":-1,"width":u64::MAX})] {
            assert!(s.execute(command, params).is_err());
        }
    }
    assert!(s.execute("ai.configure", json!({"settings":{"serverUrl":"file:///tmp/server"}})).is_err());
    assert!(s.execute("ai.cancel", json!({"pendingOnly":"yes"})).is_err());
    assert_eq!(s.execute("ai.cancel", json!({"pendingOnly":true})).unwrap()["pendingOnly"], true);
    assert!(decode_result(b"bad image").is_err());
    let image = Image::from_u8(2, 2, ChannelLayout::Rgba, vec![128; 16]).unwrap();
    assert_eq!(decode_result(&encode_png(&image).unwrap()).unwrap().dimensions(), (2, 2));
}

#[test]
fn editable_mask_survives_accept_history_and_native_save() {
    let mut s = session(SampleType::U16);
    let mut c = candidate(&s);
    let mut mask = photocraft_raster::Surface::new(photocraft_color::PixelFormat { sample: SampleType::F32, ..photocraft_color::PixelFormat::GRAY8 });
    mask.write_pixel(2, 2, &[0.35]);
    c.placement.mask = Some(mask.clone());
    s.ai.candidates.push(c);
    let layer = s.execute("ai.accept", json!({"id":1,"maskEnabled":false})).unwrap()["layer"].as_u64().unwrap();
    let doc = &s.active().unwrap().doc;
    assert_eq!(doc.layer(photocraft_doc::LayerId(layer)).unwrap().mask.as_ref().unwrap().surface, mask);
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
    let restored = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(restored.layers[0].mask, doc.layers[0].mask);
    assert_eq!(restored.layers[0].psd_blocks, doc.layers[0].psd_blocks);
    s.execute("image.adjustments.invert", json!({"target":"mask","layer":layer})).unwrap();
    assert!((s.active().unwrap().doc.layers[0].mask.as_ref().unwrap().surface.sample_channel(2, 2, 0) - 0.65).abs() < 0.0001);
    assert!(s.undo());
    assert_eq!(s.active().unwrap().doc.layers[0].mask.as_ref().unwrap().surface, mask);
}

#[test]
fn closing_and_reopening_persisted_document_id_never_retargets_candidate() {
    let mut s = session(SampleType::U8);
    let original = s.active().unwrap().doc.as_ref().clone();
    s.ai.candidates.push(candidate(&s));
    s.close(0);
    s.add_document(original, None);
    assert!(s.execute("ai.accept", json!({"id":1,"allowStale":true})).is_err());
    assert_eq!(s.active().unwrap().doc.layers.len(), 0);
    assert!(s.ai.candidates[0].placement.document_closed);
}
