//! Native modeless generation panel. All actions dispatch engine commands; previews are cached.
use std::collections::BTreeMap;

use egui::{ColorImage, RichText, TextureHandle, TextureOptions, vec2};
use photocraft_engine::ai::{Mode, Request, Settings, Source, workflow::Workflow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{PhotocraftApp, theme::Tokens, widgets};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AiUi {
    pub open: bool,
    pub request: Request,
    pub random_seed: bool,
    pub fixed_seed: u32,
    pub auto_resolution: bool,
    pub count_choice: u32,
    pub custom_count: u32,
    pub settings: Option<Settings>,
    pub template_json: String,
    pub error: String,
    pub preview_mask: bool,
    pub accept_mask: bool,
    pub allow_stale: bool,
    #[serde(skip)]
    pub token: String,
}
impl Default for AiUi {
    fn default() -> Self {
        Self {
            open: false,
            request: Request { width: 512, height: 512, ..Default::default() },
            random_seed: true,
            fixed_seed: 0,
            auto_resolution: true,
            count_choice: 1,
            custom_count: 3,
            settings: None,
            template_json: String::new(),
            error: String::new(),
            preview_mask: false,
            accept_mask: true,
            allow_stale: false,
            token: String::new(),
        }
    }
}
#[derive(Default)]
pub struct Previews {
    textures: BTreeMap<(u64, bool), TextureHandle>,
    crop: Option<CropPreview>,
}
struct CropPreview {
    document: std::sync::Weak<photocraft_doc::Document>,
    revision: u64,
    padding: u32,
    rect: Result<photocraft_geom::Rect, String>,
}

/// Change form state without submitting work or applying preferences. Authentication is an
/// engine command, so neither inspect nor form patches serialize the session token.
pub fn patch_form(current: &AiUi, patch: &Value) -> Result<AiUi, String> {
    fn merge(target: &mut Value, patch: &Value, depth: usize) -> Result<(), String> {
        if depth > 8 {
            return Err("AI form patch is too deeply nested".into());
        }
        if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
            for (key, value) in patch {
                merge(target.entry(key.clone()).or_insert(Value::Null), value, depth + 1)?;
            }
        } else {
            *target = patch.clone();
        }
        Ok(())
    }
    if !patch.is_object() || patch.get("token").is_some() {
        return Err("AI form must be an object; configure authentication with ai.configure".into());
    }
    if serde_json::to_vec(patch).map_err(|e| e.to_string())?.len() > 4 << 20 {
        return Err("AI form exceeds 4 MB".into());
    }
    let mut value = serde_json::to_value(current).map_err(|e| e.to_string())?;
    merge(&mut value, patch, 0)?;
    let mut form: AiUi = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if ![0, 1, 2, 4].contains(&form.count_choice)
        || !(1..=photocraft_engine::ai::MAX_COUNT).contains(&form.custom_count)
        || form.request.prompt.len() > 16384
        || form.template_json.len() > 2 << 20
        || form.request.width > 8192
        || form.request.height > 8192
        || u64::from(form.request.width) * u64::from(form.request.height) > photocraft_engine::ai::MAX_PIXELS
    {
        return Err("invalid AI form count, prompt, template or dimensions".into());
    }
    if let Some(settings) = &form.settings {
        settings.validate().map_err(|e| e.to_string())?;
    }
    form.token.clone_from(&current.token);
    Ok(form)
}

pub fn menu(app: &mut PhotocraftApp, params: &Value) -> Result<Value, String> {
    app.ui.ai.open = params.get("show").and_then(Value::as_bool).unwrap_or(!app.ui.ai.open);
    if app.ui.ai.open {
        app.ui.ai.settings = Some(app.session.prefs().ai.clone());
    }
    Ok(json!({"open":app.ui.ai.open}))
}
/// Poll workers even with a hidden panel; never run a generation from repaint code.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.session.ai.tick();
    if app.session.ai.status.running {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    app.ai_previews.textures.retain(|(id, _), _| app.session.ai.candidates.iter().any(|c| c.id == *id));
}
fn cache(app: &mut PhotocraftApp, ctx: &egui::Context) {
    for candidate in &app.session.ai.candidates {
        for mask in [false, true] {
            app.ai_previews.textures.entry((candidate.id, mask)).or_insert_with(|| {
                let image = &candidate.image;
                let scale = (256.0 / image.width().max(image.height()) as f64).min(1.0);
                let width = ((image.width() as f64 * scale) as usize).max(1);
                let height = ((image.height() as f64 * scale) as usize).max(1);
                let mut pixels = Vec::with_capacity(width * height * 4);
                let profile =
                    image.icc.as_ref().and_then(|b| photocraft_cms::Profile::parse(b).ok()).unwrap_or_else(|| photocraft_cms::Builtin::Srgb.profile().clone());
                let transform =
                    photocraft_cms::Transform::new(&profile, photocraft_cms::Builtin::Srgb.profile(), photocraft_cms::Intent::RelativeColorimetric, true).ok();
                for y in 0..height {
                    for x in 0..width {
                        let ix = (x as u64 * u64::from(image.width()) / width as u64) as u32;
                        let iy = (y as u64 * u64::from(image.height()) / height as u64) as u32;
                        let raw = [image.get(ix, iy, 0), image.get(ix, iy, 1), image.get(ix, iy, 2)];
                        let mut rgb = raw;
                        if let Some(t) = &transform {
                            t.eval(&raw, &mut rgb);
                        }
                        let mut alpha = if image.layout().has_alpha() { image.get(ix, iy, 3) } else { 1.0 };
                        if mask && let Some(selection) = &candidate.placement.mask {
                            let rect = candidate.placement.rect;
                            let dx = rect.x0 + (x as u64 * u64::from(rect.width()) / width as u64) as i32;
                            let dy = rect.y0 + (y as u64 * u64::from(rect.height()) / height as u64) as i32;
                            alpha *= selection.sample_channel(dx, dy, 0);
                        }
                        pixels.extend(rgb.into_iter().chain([alpha]).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8));
                    }
                }
                ctx.load_texture(
                    format!("ai-preview-{}-{mask}", candidate.id),
                    ColorImage::from_rgba_unmultiplied([width, height], &pixels),
                    TextureOptions::LINEAR,
                )
            });
        }
    }
}
fn command(app: &mut PhotocraftApp, panel: &mut AiUi, id: &str, params: Value) {
    match app.run(id, params) {
        Ok(_) => panel.error.clear(),
        Err(error) => panel.error = error,
    }
}
fn apply_settings(app: &mut PhotocraftApp, panel: &mut AiUi) -> bool {
    let mut params = json!({"settings":panel.settings});
    if !panel.token.is_empty() {
        params["token"] = json!(panel.token);
    }
    command(app, panel, "ai.configure", params);
    if panel.error.is_empty() {
        panel.token.clear();
        true
    } else {
        false
    }
}
fn workflow_defaults(panel: &mut AiUi, w: &Workflow) {
    let value = |key: &str| w.bindings.get(key).and_then(|paths| paths.first()).and_then(|p| w.graph.pointer(p));
    panel.request.steps = value("steps").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok());
    panel.request.guidance = value("guidance").and_then(Value::as_f64).map(|v| v as f32);
    panel.request.strength = value("strength").and_then(Value::as_f64).map(|v| v as f32);
    if !w.modes.contains(&panel.request.mode) {
        panel.request.mode = w.modes.first().copied().unwrap_or(Mode::Generate);
    }
}

fn submit_form(app: &mut PhotocraftApp, panel: &mut AiUi, workflow: &Workflow, new_seed: bool) {
    panel.request.seed = if !new_seed && !panel.random_seed && workflow.bindings.contains_key("seed") { Some(panel.fixed_seed) } else { None };
    let mut request = panel.request.clone();
    if request.mode != Mode::Generate && panel.auto_resolution {
        request.width = 0;
        request.height = 0;
    }
    if apply_settings(app, panel) {
        command(app, panel, "ai.generate", serde_json::to_value(request).unwrap_or(Value::Null));
    }
}

fn show_results(app: &mut PhotocraftApp, panel: &mut AiUi, ui: &mut egui::Ui, t: &Tokens) {
    if app.session.ai.status.running {
        ui.spinner();
        ui.label(format!("Completed {} / {}", app.session.ai.status.completed, app.session.ai.status.total));
        ui.add_enabled_ui(app.session.ai.status.total.saturating_sub(app.session.ai.status.completed) > 1, |ui| {
            if widgets::secondary_button(ui, "Cancel remaining jobs", 180.0).clicked() {
                command(app, panel, "ai.cancel", json!({"pendingOnly":true}));
            }
        });
        if widgets::secondary_button(ui, "Cancel running and pending jobs", 180.0).clicked() {
            command(app, panel, "ai.cancel", json!({}));
        }
    }
    if !panel.error.is_empty() {
        ui.colored_label(t.warning, &panel.error);
    }
    if !app.session.ai.candidates.is_empty() {
        widgets::hairline(ui);
        widgets::checkbox(ui, &mut panel.preview_mask, "Preview selection mask");
        widgets::checkbox(ui, &mut panel.accept_mask, "Enable selection mask on accepted layers");
        let stale = app
            .session
            .ai
            .candidates
            .iter()
            .any(|c| app.session.active().is_some_and(|st| st.doc.id == c.placement.document && st.revision != c.placement.revision));
        if stale {
            ui.colored_label(t.warning, "The document changed. Review the original placement before acceptance.");
            widgets::checkbox(ui, &mut panel.allow_stale, "I reviewed the candidate; use the original placement");
        } else {
            panel.allow_stale = false;
        }
        let rows: Vec<_> =
            app.session.ai.candidates.iter().map(|c| (c.id, c.placement.document, c.placement.document_closed, c.metadata.get("seed").cloned())).collect();
        for (id, document, closed, seed) in rows {
            let label =
                seed.as_ref().and_then(Value::as_u64).map_or_else(|| format!("Candidate {id} · workflow seed"), |seed| format!("Candidate {id} · seed {seed}"));
            ui.label(RichText::new(label).strong());
            if let Some(texture) = app.ai_previews.textures.get(&(id, panel.preview_mask)) {
                ui.add(egui::Image::new(texture).max_size(vec2(256.0, 256.0)));
            }
            let correct_document = !closed && app.session.active().is_some_and(|st| st.doc.id == document);
            if !correct_document {
                ui.small(if closed {
                    "Original document was closed. Retrieve this result from InvokeAI or discard it."
                } else {
                    "Activate the original document to accept this result."
                });
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(correct_document && (!stale || panel.allow_stale), |ui| {
                    if widgets::primary_button(ui, "Accept as layer", 110.0).clicked() {
                        let params = json!({"id":id,"maskEnabled":panel.accept_mask,"allowStale":panel.allow_stale});
                        command(app, panel, "ai.accept", params);
                    }
                });
                if widgets::secondary_button(ui, "Discard", 70.0).clicked() {
                    command(app, panel, "ai.discard", json!({"id":id}));
                }
            });
        }
        if panel.settings.as_ref().and_then(|s| s.workflows.get(s.selected_workflow)).is_some_and(|w| w.bindings.contains_key("seed")) {
            ui.small("Accept or discard candidates to enable regeneration with a new seed.");
        }
    }
}

pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.ai.open {
        return;
    }
    cache(app, ctx);
    let mut panel = std::mem::take(&mut app.ui.ai);
    let mut open = panel.open;
    if panel.settings.is_none() {
        panel.settings = Some(app.session.prefs().ai.clone());
    }
    let t = Tokens::get(ctx);
    egui::Window::new(tl!("AI Generation"))
        .id(egui::Id::new("local-ai-generation"))
        .open(&mut open)
        .default_width(390.0)
        .default_height(660.0)
        .resizable(true)
        .movable(!app.session.prefs().workspace_locked)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 100.0).clamp(180.0, 640.0)).show(ui, |ui| {
                if cfg!(target_arch = "wasm32") {
                    ui.label("Local InvokeAI is unavailable in the web build.");
                    return;
                }
                ui.label(&app.session.ai.status.message);
                show_results(app, &mut panel, ui, &t);
                ui.collapsing("Connection and executable workflows", |ui| {
                    if let Some(settings) = &mut panel.settings {
                        ui.label("InvokeAI server URL");
                        ui.text_edit_singleline(&mut settings.server_url);
                        ui.label("Bearer token (session only)");
                        ui.add(egui::TextEdit::singleline(&mut panel.token).password(true));
                        if !app.session.ai.token.is_empty() {
                            ui.small("Session authentication configured");
                        }
                        ui.horizontal(|ui| {
                            ui.label("HTTP timeout (seconds)");
                            ui.add(egui::DragValue::new(&mut settings.request_timeout_secs).range(1..=120));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Job timeout (seconds)");
                            ui.add(egui::DragValue::new(&mut settings.job_timeout_secs).range(1..=86400));
                        });
                    }
                    ui.label("Paste a PhotoCraft executable graph template JSON");
                    ui.add(egui::TextEdit::multiline(&mut panel.template_json).desired_rows(4).char_limit(2 << 20).desired_width(f32::INFINITY));
                    if widgets::secondary_button(ui, "Import template", 100.0).clicked() {
                        match serde_json::from_str::<Workflow>(&panel.template_json)
                            .map_err(|e| e.to_string())
                            .and_then(|w| w.validate().map(|_| w).map_err(|e| e.to_string()))
                        {
                            Ok(w) => {
                                workflow_defaults(&mut panel, &w);
                                if let Some(settings) = &mut panel.settings {
                                    if settings.workflows.len() < 32 {
                                        settings.workflows.push(w);
                                        settings.selected_workflow = settings.workflows.len() - 1;
                                        panel.template_json.clear();
                                        panel.error.clear();
                                    } else {
                                        panel.error = "At most 32 workflows are supported".into();
                                    }
                                }
                            }
                            Err(e) => panel.error = e,
                        }
                    }
                    ui.add_enabled_ui(!app.session.ai.status.running, |ui| {
                        if widgets::secondary_button(ui, "Save settings", 100.0).clicked() {
                            apply_settings(app, &mut panel);
                        }
                        if widgets::secondary_button(ui, "Test connection", 100.0).clicked() && apply_settings(app, &mut panel) {
                            command(app, &mut panel, "ai.connect", json!({}));
                        }
                    });
                });
                ui.label("Model / workflow");
                let mut selected_changed = false;
                if let Some(settings) = &mut panel.settings {
                    let names: Vec<_> = settings.workflows.iter().enumerate().map(|(i, w)| (i, w.name.as_str())).collect();
                    if names.is_empty() {
                        ui.label("Import a working executable template to enable generation.");
                    } else {
                        selected_changed = widgets::dropdown(ui, "ai-workflow", &mut settings.selected_workflow, &names, 300.0);
                    }
                }
                let workflow = panel.settings.as_ref().and_then(|s| s.workflows.get(s.selected_workflow)).cloned();
                if let Some(w) = &workflow {
                    if selected_changed || !w.modes.contains(&panel.request.mode) {
                        workflow_defaults(&mut panel, w);
                    }
                    let modes: Vec<_> = w
                        .modes
                        .iter()
                        .map(|m| {
                            (
                                *m,
                                match m {
                                    Mode::Generate => "Generate",
                                    Mode::Edit => "Edit / Transform",
                                    Mode::MasklessFill => "Generative Fill — maskless",
                                    Mode::Inpaint => "Generative Fill — true inpainting",
                                },
                            )
                        })
                        .collect();
                    widgets::dropdown(ui, "ai-mode", &mut panel.request.mode, &modes, 300.0);
                    if panel.request.mode != Mode::Generate {
                        widgets::dropdown(
                            ui,
                            "ai-source",
                            &mut panel.request.source,
                            &[(Source::ActiveLayer, "Active layer"), (Source::MergedVisible, "Merged visible")],
                            300.0,
                        );
                    }
                    if let Some(st) = app.session.active() {
                        if st.doc.selection.is_some() {
                            let padding = panel.settings.as_ref().map_or(0, |s| s.context_padding);
                            let cache = &mut app.ai_previews.crop;
                            let document = std::sync::Arc::downgrade(&st.doc);
                            if cache.as_ref().is_none_or(|c| !c.document.ptr_eq(&document) || c.revision != st.revision || c.padding != padding) {
                                *cache = Some(CropPreview {
                                    document,
                                    revision: st.revision,
                                    padding,
                                    rect: photocraft_engine::ai::images::crop_rect(&st.doc, padding).map_err(|e| e.to_string()),
                                });
                            }
                            match cache.as_ref().map(|c| &c.rect) {
                                Some(Ok(r)) => {
                                    ui.small(format!("Selection crop: {},{} · {}×{}", r.x0, r.y0, r.width(), r.height()));
                                }
                                Some(Err(e)) => {
                                    ui.colored_label(t.warning, e.to_string());
                                }
                                None => {}
                            }
                        } else {
                            ui.small(if panel.request.mode == Mode::Generate {
                                "Text to image; no reference uploaded"
                            } else {
                                "No selection: full chosen source"
                            });
                        }
                    }
                    ui.label("Prompt / editing instructions");
                    ui.add(egui::TextEdit::multiline(&mut panel.request.prompt).desired_rows(4).char_limit(16384).desired_width(f32::INFINITY));
                    widgets::dropdown(
                        ui,
                        "ai-count",
                        &mut panel.count_choice,
                        &[(1, "1 generation"), (2, "2 generations"), (4, "4 generations"), (0, "Custom count")],
                        300.0,
                    );
                    if panel.count_choice == 0 {
                        ui.add(egui::DragValue::new(&mut panel.custom_count).range(1..=16));
                    }
                    let count = if panel.count_choice == 0 { panel.custom_count } else { panel.count_choice };
                    panel.request.count = Some(count);
                    ui.small(format!("{count} sequential job(s). Local inference can take several minutes each."));
                    ui.collapsing("Advanced settings", |ui| {
                        if w.bindings.contains_key("seed") {
                            widgets::checkbox(ui, &mut panel.random_seed, "Random seed");
                            if !panel.random_seed {
                                ui.horizontal(|ui| {
                                    ui.label("Seed");
                                    ui.add(egui::DragValue::new(&mut panel.fixed_seed).range(0..=u32::MAX));
                                });
                            }
                        }
                        if panel.request.mode != Mode::Generate {
                            widgets::checkbox(ui, &mut panel.auto_resolution, "Derive model resolution from crop");
                        }
                        if panel.request.mode == Mode::Generate || !panel.auto_resolution {
                            ui.horizontal(|ui| {
                                ui.label("Width × height");
                                ui.add(egui::DragValue::new(&mut panel.request.width).range(1..=8192));
                                ui.add(egui::DragValue::new(&mut panel.request.height).range(1..=8192));
                            });
                        }
                        ui.small(format!("Workflow limit: {}×{}; multiples of {}", w.max_width, w.max_height, w.dimension_multiple));
                        if w.bindings.contains_key("steps") {
                            let steps = panel.request.steps.get_or_insert(30);
                            ui.horizontal(|ui| {
                                ui.label("Steps");
                                ui.add(egui::DragValue::new(steps).range(1..=500));
                            });
                        }
                        if w.bindings.contains_key("guidance") {
                            let guidance = panel.request.guidance.get_or_insert(3.5);
                            widgets::slider_row(ui, "Guidance", guidance, 0.0..=100.0, "", None);
                        }
                        if w.bindings.contains_key("strength") {
                            let strength = panel.request.strength.get_or_insert(0.75);
                            widgets::slider_row(ui, "Strength", strength, 0.0..=1.0, "", None);
                        }
                        if let Some(settings) = &mut panel.settings {
                            ui.horizontal(|ui| {
                                ui.label("Selection context padding");
                                ui.add(egui::DragValue::new(&mut settings.context_padding).range(0..=2048).suffix(" px"));
                            });
                        }
                        if panel.request.mode == Mode::Inpaint {
                            ui.small(format!("Workflow mask convention: {:?}", w.mask_semantics));
                        }
                    });
                    let ready = !app.session.ai.status.running
                        && app.session.ai.candidates.is_empty()
                        && app.session.is_enabled("ai.generate")
                        && !panel.request.prompt.trim().is_empty();
                    ui.add_enabled_ui(ready, |ui| {
                        if widgets::primary_button(ui, "Generate", 140.0).clicked() {
                            submit_form(app, &mut panel, w, false);
                        }
                        if w.bindings.contains_key("seed") && widgets::secondary_button(ui, "Regenerate with new seed", 180.0).clicked() {
                            submit_form(app, &mut panel, w, true);
                        }
                    });
                }
            });
        });
    panel.open = open;
    app.ui.ai = panel;
}

#[cfg(test)]
mod tests;
