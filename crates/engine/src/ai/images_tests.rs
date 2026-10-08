#![allow(clippy::unwrap_used)] // Synthetic image setup helpers.
use super::images::*;
use super::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

fn document() -> Document {
    let mut d = Document::new("test", Size::new(100, 80), ColorMode::Rgb, SampleType::U16);
    let mut layer = Layer::raster("source", d.pixel_format());
    layer.surface_mut().unwrap().fill_rect(Rect::from_xywh(20, 10, 60, 60), &[0.5, 0.2, 0.1, 0.7]);
    d.layers.push(layer);
    d
}
fn workflow() -> workflow::Workflow {
    let mut w = crate::ai_cmds::tests::workflow();
    w.modes = vec![Mode::Generate, Mode::Edit, Mode::MasklessFill];
    w.graph["nodes"]["out"]["image"] = serde_json::json!({"image_name":"source.png"});
    w.bindings.insert("reference".into(), vec!["/nodes/out/image".into()]);
    w.max_width = 32;
    w.max_height = 32;
    w
}
#[test]
fn cropped_padded_irregular_feathered_mask_and_original_coordinates() {
    let mut d = document();
    let mut selection = Surface::new(PixelFormat { sample: SampleType::F32, ..PixelFormat::GRAY8 });
    selection.write_pixel(35, 25, &[0.25]);
    selection.write_pixel(44, 34, &[0.8]);
    d.selection = Some(selection);
    assert_eq!(crop_rect(&d, 5).unwrap(), Rect::from_xywh(30, 20, 20, 20));
    let request = Request { mode: Mode::MasklessFill, prompt: "test".into(), ..Default::default() };
    let p = prepare(&d, 9, d.top_layer(), &request, &workflow(), 5).unwrap();
    assert_eq!(p.placement.revision, 9);
    assert_eq!(p.placement.rect, Rect::from_xywh(30, 20, 20, 20));
    let reference = decode_result(p.reference.as_ref().unwrap()).unwrap();
    assert_eq!(reference.dimensions(), (20, 20));
    assert!(reference.get(5, 5, 3) > 0.69);
    let mask = p.placement.mask.as_ref().unwrap();
    assert_eq!(mask.sample_channel(35, 25, 0), 0.25);
    assert_eq!(mask.sample_channel(44, 34, 0), 0.8);
    assert_eq!(mask.sample_channel(36, 26, 0), 0.0);
    assert_eq!(mask.sample_channel(29, 19, 0), 0.0);
    let layer = result_layer(&d, &p.placement, &reference, serde_json::json!({}), true).unwrap();
    assert_eq!(layer.mask.as_ref().unwrap().surface, mask.clone());
    assert!(layer.surface().unwrap().sample_channel(35, 25, 3) > 0.69);
    assert_eq!(layer.surface().unwrap().sample_channel(5, 5, 3), 0.0);
    let unmasked = result_layer(&d, &p.placement, &reference, serde_json::json!({}), false).unwrap();
    assert!(!unmasked.mask.unwrap().enabled);
}
#[test]
fn edges_empty_outside_and_no_selection_distinction() {
    let mut d = document();
    let mut selection = Surface::new(PixelFormat::GRAY8);
    selection.fill_rect(Rect::from_xywh(0, 0, 4, 3), &[1.0]);
    d.selection = Some(selection);
    assert_eq!(crop_rect(&d, 32).unwrap(), Rect::from_xywh(0, 0, 36, 35));
    let edit = Request { mode: Mode::Edit, prompt: "test".into(), ..Default::default() };
    assert!(prepare(&d, 1, d.top_layer(), &edit, &workflow(), 0).is_err());
    d.selection = Some(Surface::new(PixelFormat::GRAY8));
    assert!(crop_rect(&d, 32).is_err());
    let mut outside = Surface::new(PixelFormat::GRAY8);
    outside.fill_rect(Rect::from_xywh(200, 200, 10, 10), &[1.0]);
    d.selection = Some(outside);
    assert!(crop_rect(&d, 32).is_err());
    d.selection = None;
    let p = prepare(&d, 1, d.top_layer(), &edit, &workflow(), 32).unwrap();
    assert_eq!(p.placement.rect, d.bounds());
    assert_eq!((p.width, p.height), (32, 25));
    assert!(p.reference.is_some());
    assert!(p.placement.mask.is_none());
    let generate = Request { mode: Mode::Generate, prompt: "test".into(), width: 16, height: 16, ..Default::default() };
    let p = prepare(&d, 1, d.top_layer(), &generate, &workflow(), 32).unwrap();
    assert!(p.reference.is_none());
    assert_eq!(p.placement.rect, Rect::from_xywh(0, 0, 16, 16));
    let fill = Request { mode: Mode::MasklessFill, ..edit };
    assert!(prepare(&d, 1, d.top_layer(), &fill, &workflow(), 0).is_err());
}
#[test]
fn default_selection_mapping_resize_and_merged_without_flattening() {
    let mut d = document();
    d.selection = Some(Surface::with_default(PixelFormat::GRAY8, &[1.0]));
    assert_eq!(crop_rect(&d, 0).unwrap(), d.bounds());
    let snapshot = d.clone();
    let request = Request { mode: Mode::Edit, source: Source::MergedVisible, prompt: "test".into(), ..Default::default() };
    let p = prepare(&d, 1, None, &request, &workflow(), 0).unwrap();
    assert_eq!(d, snapshot);
    let image = decode_result(p.reference.as_ref().unwrap()).unwrap();
    assert_eq!(image.dimensions(), (32, 25));
    let layer = result_layer(&d, &p.placement, &image, serde_json::json!({}), true).unwrap();
    assert!(layer.surface().unwrap().sample_channel(45, 35, 3) > 0.65);
    assert!(layer.mask.unwrap().value(99, 79) > 0.99);
    let mut mask = Surface::new(PixelFormat { sample: SampleType::F32, ..PixelFormat::GRAY8 });
    mask.write_pixel(20, 20, &[f32::NAN]);
    d.selection = Some(mask);
    assert!(prepare(&d, 1, d.top_layer(), &request, &workflow(), 0).is_err());
}
