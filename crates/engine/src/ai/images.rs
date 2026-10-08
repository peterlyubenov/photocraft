//! Model RGB boundary and immutable request placement. Editor surfaces keep their native depth.
use std::sync::Arc;

use photocraft_cms::{Builtin, Intent, Profile, Transform};
use photocraft_codecs::{DecodeOptions, Format, Image, Limits};
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

pub fn prepare(
    doc: &Document,
    revision: u64,
    _active: Option<photocraft_doc::LayerId>,
    request: &Request,
    workflow: &Workflow,
    _padding: u32,
) -> AiResult<Prepared> {
    request.validate(workflow)?;
    if request.mode != Mode::Generate {
        return Err(AiError::Invalid("reference modes are not implemented yet".into()));
    }
    let (width, height) = workflow.dimensions(request.width, request.height)?;
    let rect = Rect::from_xywh(0, 0, width, height);
    Ok(Prepared {
        placement: Placement {
            document: doc.id,
            revision,
            canvas: doc.size,
            mode: doc.mode,
            depth: doc.depth,
            profile: doc.icc_profile.clone(),
            rect,
            mask: None,
        },
        reference: None,
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
    let metadata = json!({"version": 1, "generation": metadata, "placement": placement.rect, "source": Source::ActiveLayer});
    layer.psd_blocks.push((*b"pcAI", Arc::new(serde_json::to_vec(&metadata).map_err(|e| AiError::Invalid(e.to_string()))?)));
    Ok(layer)
}
