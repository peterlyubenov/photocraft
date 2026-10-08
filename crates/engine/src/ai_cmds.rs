//! Additive local-AI commands. Only acceptance changes the document.
use crate::{
    EngineError, Result, Session,
    ai::{Request, Settings},
    commands::{CommandSpec, always},
};
use serde_json::{Value, json};

fn bad(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn doc(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or_else(|| "no document open".to_string())?;
    if st.floating.is_some() {
        return Err("finish or cancel the floating selection before AI generation/acceptance".into());
    }
    Ok(())
}
fn id(p: &Value) -> Result<u64> {
    p.get("id").and_then(Value::as_u64).ok_or_else(|| bad("candidate id is required"))
}
fn configure(s: &mut Session, p: &Value) -> Result<Value> {
    if s.ai.status.running {
        return Err(bad("wait for or cancel the current AI operation before changing settings"));
    }
    if let Some(settings) = p.get("settings") {
        if serde_json::to_vec(settings).map_err(|e| bad(e.to_string()))?.len() > 2 << 20 {
            return Err(bad("AI settings exceed 2 MB"));
        }
        let settings: Settings = serde_json::from_value(settings.clone()).map_err(|e| bad(format!("AI settings: {e}")))?;
        settings.validate()?;
        s.edit_prefs(|p| p.ai = settings);
    }
    if let Some(token) = p.get("token") {
        let token = token.as_str().filter(|t| t.len() <= 8192 && !t.contains(['\r', '\n'])).ok_or_else(|| bad("invalid bearer token"))?;
        s.ai.token = token.into();
    }
    Ok(json!({"settings": s.prefs().ai, "hasToken": !s.ai.token.is_empty()}))
}
fn connect(s: &mut Session, _: &Value) -> Result<Value> {
    let settings = s.prefs().ai.clone();
    s.ai.connect(settings)?;
    Ok(json!({"started": true}))
}
fn generate(s: &mut Session, p: &Value) -> Result<Value> {
    let request: Request = serde_json::from_value(p.clone()).map_err(|e| bad(format!("AI request: {e}")))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (snapshot, revision, active) = (st.doc.clone(), st.revision, st.active_layer);
    let settings = s.prefs().ai.clone();
    s.ai.start(snapshot, revision, active, request, settings)?;
    Ok(json!({"started": true, "count": s.ai.status.total}))
}
fn status(s: &mut Session, _: &Value) -> Result<Value> {
    s.ai.tick();
    Ok(
        json!({"queue": s.ai.status, "results": s.ai.candidates.iter().map(|c| json!({"id": c.id, "document": c.placement.document.0, "documentClosed": c.placement.document_closed, "rect": c.placement.rect, "width": c.image.width(), "height": c.image.height(), "metadata": c.metadata})).collect::<Vec<_>>()}),
    )
}
fn cancel(s: &mut Session, p: &Value) -> Result<Value> {
    let pending_only = match p.get("pendingOnly") {
        Some(v) => v.as_bool().ok_or_else(|| bad("pendingOnly must be a boolean"))?,
        None => false,
    };
    if pending_only {
        s.ai.cancel_pending();
    } else {
        s.ai.cancel();
    }
    Ok(json!({"cancellationRequested": true, "pendingOnly":pending_only}))
}
fn discard(s: &mut Session, p: &Value) -> Result<Value> {
    let id = id(p)?;
    let index = s.ai.candidates.iter().position(|c| c.id == id).ok_or_else(|| bad("no such AI candidate"))?;
    s.ai.candidates.remove(index);
    Ok(Value::Null)
}
fn accept(s: &mut Session, p: &Value) -> Result<Value> {
    let id = id(p)?;
    let index = s.ai.candidates.iter().position(|c| c.id == id).ok_or_else(|| bad("no such AI candidate"))?;
    let candidate = s.ai.candidates.get(index).ok_or_else(|| bad("no such AI candidate"))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let placement = &candidate.placement;
    if placement.document_closed {
        return Err(bad("original document was closed; retrieve this image from InvokeAI or discard it. Reopened documents need a new request"));
    }
    if st.doc.id != placement.document {
        return Err(bad("activate the original document before accepting this candidate"));
    }
    if st.doc.size != placement.canvas || st.doc.mode != placement.mode || st.doc.depth != placement.depth || st.doc.icc_profile != placement.profile {
        return Err(bad("original canvas size, mode, depth or profile changed; this candidate cannot be placed safely"));
    }
    if st.revision != placement.revision && p.get("allowStale").and_then(Value::as_bool) != Some(true) {
        return Err(bad("document changed during generation; review the candidate and explicitly confirm original placement with allowStale:true"));
    }
    let layer = crate::ai::images::result_layer(
        &st.doc,
        placement,
        &candidate.image,
        candidate.metadata.clone(),
        p.get("maskEnabled").and_then(Value::as_bool).unwrap_or(true),
    )?;
    let layer_id = layer.id;
    s.edit("Accept AI Generation", move |doc, active| {
        doc.layers.push(layer);
        *active = Some(layer_id);
        Ok(())
    })?;
    s.ai.candidates.remove(index);
    if let Some(st) = s.active() {
        let (document, revision) = (st.doc.id, st.revision);
        s.ai.accepted_revision(document, revision);
    }
    Ok(json!({"layer": layer_id.0}))
}

pub fn specs() -> Vec<CommandSpec> {
    [
        ("ai.configure", "Configure Local AI", "{settings?:{serverUrl,requestTimeoutSecs,jobTimeoutSecs,contextPadding,workflows,selectedWorkflow},token?:string}", always as fn(&Session) -> _, configure as fn(&mut Session, &Value) -> _, false),
        ("ai.connect", "Test InvokeAI Connection", "{}", always, connect, false),
        ("ai.generate", "Generate Local AI Image", "{prompt:string,mode:generate|edit|masklessFill|inpaint,source:activeLayer|mergedVisible,width:integer,height:integer,seed?:u32,steps?:u32,guidance?:number,strength?:number,count?:1..16=1}", doc, generate, false),
        ("ai.status", "Local AI Queue and Candidates", "{}", always, status, false),
        ("ai.cancel", "Cancel Local AI Queue", "{pendingOnly?:bool=false} (true lets the current submitted item finish)", always, cancel, false),
        ("ai.discard", "Discard AI Candidate", "{id:u64}", always, discard, false),
        ("ai.accept", "Accept AI Candidate as Layer", "{id:u64,allowStale?:bool=false,maskEnabled?:bool=true}", doc, accept, true),
    ].into_iter().map(|(id,label,params,enabled,run,journal)| CommandSpec { id,label,params,enabled,run,journal,menu:&[],shortcut:None }).collect()
}

#[cfg(test)]
pub(crate) mod tests;
