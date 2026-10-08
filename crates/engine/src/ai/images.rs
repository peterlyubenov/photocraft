//! Model RGB boundary and immutable request placement. Editor surfaces keep their native depth.
use std::sync::Arc;

use photocraft_cms::{Builtin, Intent, Profile, Transform};
use photocraft_codecs::{ChannelLayout, DecodeOptions, Format, Image, Limits};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::json;

use super::{AiError, AiResult, Mode, Request, Source, check_size, workflow::Workflow};

#[derive(Clone, Debug)]
pub struct Placement {
    pub document: DocId,
    pub revision: u64,
    pub canvas: photocraft_doc::Size,
    pub mode: ColorMode,
    pub depth: SampleType,
    pub profile: Option<Arc<Vec<u8>>>,
    pub rect: Rect,
    pub mask: Option<Surface>,
}
pub struct Prepared {
    pub placement: Placement,
    pub reference: Option<Vec<u8>>,
    pub mask: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

/// Selection extent in canvas coordinates. Untouched coverage is meaningful for inverted masks.
pub fn crop_rect(doc: &Document, padding: u32) -> AiResult<Rect> {
    if doc.size.width == 0 || doc.size.height == 0 || doc.size.width > i32::MAX as u32 || doc.size.height > i32::MAX as u32 || padding > 2048 {
        return Err(AiError::Invalid("invalid canvas size or context padding".into()));
    }
    let canvas = doc.bounds();
    let rect = if let Some(selection) = &doc.selection {
        if selection.channels() != 1 {
            return Err(AiError::Invalid("selection must be a grayscale coverage mask".into()));
        }
        let default = selection.default_pixel().first().copied().unwrap_or(0.0);
        if !default.is_finite() || !(0.0..=1.0).contains(&default) {
            return Err(AiError::Invalid("invalid selection coverage".into()));
        }
        let bounds = if default > 0.0 { canvas } else { selection.content_bounds().intersect(&canvas) };
        if bounds.is_empty() {
            return Err(AiError::Invalid("active selection is empty or outside the canvas".into()));
        }
        bounds.inflate(padding as i32).intersect(&canvas)
    } else {
        canvas
    };
    check_crop(rect)?;
    Ok(rect)
}
fn check_crop(rect: Rect) -> AiResult<()> {
    if rect.is_empty() || rect.width() > 16384 || rect.height() > 16384 || u64::from(rect.width()) * u64::from(rect.height()) > 67_108_864 {
        return Err(AiError::Invalid("reference/placement exceeds 64 MP or 16384 pixels per side; select a smaller region".into()));
    }
    Ok(())
}

pub fn prepare(
    doc: &Document,
    revision: u64,
    active: Option<photocraft_doc::LayerId>,
    request: &Request,
    workflow: &Workflow,
    padding: u32,
) -> AiResult<Prepared> {
    request.validate(workflow)?;
    workflow.validate()?;
    if request.mode == Mode::Inpaint {
        return Err(AiError::Invalid("true inpainting is not implemented yet".into()));
    }
    if matches!(request.mode, Mode::MasklessFill | Mode::Inpaint) && doc.selection.is_none() {
        return Err(AiError::Invalid("Generative Fill requires an active selection".into()));
    }
    let reference_needed = request.mode != Mode::Generate;
    // Selection-aware Generate still needs no reference upload, but retains original placement.
    let rect = if doc.selection.is_some() || reference_needed {
        crop_rect(doc, padding)?
    } else {
        check_size(request.width, request.height)?;
        Rect::from_xywh(0, 0, request.width, request.height)
    };
    let (width, height) = if request.width != 0 || request.height != 0 {
        workflow.dimensions(request.width, request.height)?
    } else {
        workflow.dimensions(rect.width(), rect.height())?
    };
    let mut saved_mask = doc.selection.as_ref().map(|_| Surface::new(PixelFormat { mode: ColorMode::Grayscale, sample: SampleType::F32, alpha: false }));
    if let (Some(selection), Some(mask)) = (&doc.selection, &mut saved_mask) {
        let mut nonempty = false;
        for y in rect.y0..rect.y1 {
            let row = selection.read_region(Rect::from_xywh(rect.x0, y, rect.width(), 1));
            if row.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
                return Err(AiError::Invalid("selection contains invalid coverage".into()));
            }
            nonempty |= row.iter().any(|v| *v > 0.0);
            mask.write_region(Rect::from_xywh(rect.x0, y, rect.width(), 1), &row);
        }
        if !nonempty {
            return Err(AiError::Invalid("active selection is empty".into()));
        }
    }
    let reference = if reference_needed {
        let mut source = doc.clone();
        if request.source == Source::ActiveLayer {
            let mut layer =
                active.and_then(|id| doc.layer(id)).cloned().ok_or_else(|| AiError::Invalid("select a source layer or choose merged visible".into()))?;
            layer.visible = true;
            layer.clipped = false;
            source.layers = vec![layer];
        }
        // Composite only the request crop, in its original document coordinate space.
        let buffer = photocraft_compose::render(&source, rect);
        if request.source == Source::ActiveLayer
            && !buffer.px.iter().enumerate().any(|(i, p)| {
                let x = rect.x0 + (i % rect.width() as usize) as i32;
                let y = rect.y0 + (i / rect.width() as usize) as i32;
                p[3] > 0.0 && saved_mask.as_ref().is_none_or(|m| m.sample_channel(x, y, 0) > 0.0)
            })
        {
            return Err(AiError::Invalid("selected region has no visible source pixels; choose merged visible or another layer".into()));
        }
        let transform = Transform::new(&crate::color_cmds::composite_profile(doc), Builtin::Srgb.profile(), Intent::RelativeColorimetric, true)
            .map_err(|e| AiError::Backend(format!("reference colour conversion: {e}")))?;
        let mut rgb = Surface::new(PixelFormat { mode: ColorMode::Rgb, sample: SampleType::F32, alpha: true });
        for (row_index, row) in buffer.px.chunks(rect.width() as usize).enumerate() {
            let raw: Vec<f32> = row.iter().flat_map(|p| *p).collect();
            let mut srgb = vec![0.0; raw.len()];
            transform.convert_f32(&raw, 4, &mut srgb, 4, true);
            rgb.write_region(Rect::from_xywh(0, row_index as i32, rect.width(), 1), &srgb);
        }
        let origin = Rect::from_xywh(0, 0, rect.width(), rect.height());
        let scaled = photocraft_algo::resample::resize_surface_in_canvas(
            &rgb,
            f64::from(width) / f64::from(rect.width()),
            f64::from(height) / f64::from(rect.height()),
            photocraft_algo::resample::Resample::Bilinear,
            origin,
        );
        let model_rect = Rect::from_xywh(0, 0, width, height);
        // Models receive sRGB RGBA8 PNG; alpha and the original float selection stay separate.
        let mut image = Image::from_normalized(width, height, ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, &scaled.read_region(model_rect))
            .map_err(|e| AiError::Backend(e.to_string()))?;
        image.icc = Some(Builtin::Srgb.profile().to_bytes().to_vec());
        Some(encode_png(&image)?)
    } else {
        None
    };
    Ok(Prepared {
        placement: Placement {
            document: doc.id,
            revision,
            canvas: doc.size,
            mode: doc.mode,
            depth: doc.depth,
            profile: doc.icc_profile.clone(),
            rect,
            mask: saved_mask,
        },
        reference,
        mask: None,
        width,
        height,
    })
}

pub fn decode_result(bytes: &[u8]) -> AiResult<Image> {
    if !matches!(photocraft_codecs::detect(bytes), Some(Format::Png | Format::Jpeg)) {
        return Err(AiError::Backend("result must be PNG or JPEG".into()));
    }
    let image = photocraft_codecs::decode_with(
        bytes,
        &DecodeOptions { limits: Limits { max_width: 8192, max_height: 8192, max_pixels: super::MAX_PIXELS, max_alloc: 256 << 20 }, ..Default::default() },
    )
    .map_err(|e| AiError::Backend(format!("invalid result image: {e}")))?;
    check_size(image.width(), image.height())?;
    if !image.layout().is_rgb() {
        return Err(AiError::Backend("model result must be an RGB image".into()));
    }
    Ok(image)
}

pub fn encode_png(image: &Image) -> AiResult<Vec<u8>> {
    photocraft_codecs::encode(image, Format::Png, &Default::default()).map_err(|e| AiError::Backend(format!("PNG encoding: {e}")))
}

/// Convert result pixels to the original document profile/depth, retaining placement and mask.
pub fn result_layer(doc: &Document, placement: &Placement, image: &Image, metadata: serde_json::Value, mask_enabled: bool) -> AiResult<Layer> {
    if !matches!(doc.mode, ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Cmyk | ColorMode::Lab) {
        return Err(AiError::Invalid("AI insertion supports RGB, Grayscale, CMYK and Lab documents".into()));
    }
    check_size(image.width(), image.height())?;
    check_crop(placement.rect)?;
    let source = match &image.icc {
        Some(bytes) => Profile::parse(bytes).map_err(|e| AiError::Backend(format!("invalid result ICC profile: {e}")))?,
        None => Builtin::Srgb.profile().clone(),
    };
    if source.color_space != photocraft_cms::ColorSpace::Rgb {
        return Err(AiError::Backend("result ICC profile is not RGB".into()));
    }
    let destination = crate::color_cmds::document_profile(doc);
    let transform =
        Transform::new(&source, &destination, Intent::RelativeColorimetric, true).map_err(|e| AiError::Backend(format!("colour conversion: {e}")))?;
    let fmt = doc.pixel_format();
    let mut pixels = Surface::new(fmt);
    let src = image.to_rgba_f32();
    let mut rgb = Surface::new(PixelFormat { mode: ColorMode::Rgb, sample: SampleType::F32, alpha: true });
    let input = Rect::from_xywh(0, 0, image.width(), image.height());
    rgb.write_region(input, &src);
    let resized = photocraft_algo::resample::resize_surface_in_canvas(
        &rgb,
        f64::from(placement.rect.width()) / f64::from(image.width()),
        f64::from(placement.rect.height()) / f64::from(image.height()),
        photocraft_algo::resample::Resample::Bilinear,
        input,
    );
    // Rows bound working memory; coordinates in the request are never inferred from a later selection.
    for y in 0..placement.rect.height() {
        let row = resized.read_region(Rect::from_xywh(0, y as i32, placement.rect.width(), 1));
        let mut converted = vec![0.0; placement.rect.width() as usize * fmt.channels()];
        transform.convert_f32(&row, 4, &mut converted, fmt.channels(), true);
        pixels.write_region(Rect::from_xywh(placement.rect.x0, placement.rect.y0 + y as i32, placement.rect.width(), 1), &converted);
    }
    let mut layer = Layer::new("AI Generated", LayerContent::Raster(pixels));
    layer.mask = placement.mask.clone().map(|surface| LayerMask { surface, enabled: mask_enabled, linked: true, density: 1.0, feather: 0.0 });
    // Private additional-info block already survives .pcraft/PSD, avoiding document schema changes.
    let metadata = json!({"version": 1, "generation": metadata, "placement": placement.rect});
    layer.psd_blocks.push((*b"pcAI", Arc::new(serde_json::to_vec(&metadata).map_err(|e| AiError::Invalid(e.to_string()))?)));
    Ok(layer)
}
