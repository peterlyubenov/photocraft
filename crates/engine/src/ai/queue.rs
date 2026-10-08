//! Separate from editing jobs: inference never locks or replaces document state.
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use photocraft_codecs::Image;
use serde_json::{Value, json};

use super::{
    AiError, AiResult, Backend, JobStatus, Request, Settings,
    images::{Placement, Prepared},
    workflow::Workflow,
};

pub struct Candidate {
    pub id: u64,
    pub placement: Placement,
    pub image: Image,
    pub metadata: Value,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub message: String,
    pub completed: u32,
    pub total: u32,
    pub remote_item: Option<u64>,
}
impl Default for Status {
    fn default() -> Self {
        Self { running: false, message: "Connection not tested".into(), completed: 0, total: 0, remote_item: None }
    }
}
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Event {
    Message(String, Option<u64>),
    Candidate(Box<Candidate>),
    Finished(AiResult<()>),
}
struct Worker {
    receiver: mpsc::Receiver<Event>,
    cancel: Arc<AtomicBool>,
    cancel_pending: Arc<AtomicBool>,
}
#[derive(Default)]
pub struct Generation {
    pub status: Status,
    pub candidates: Vec<Candidate>,
    pub token: String,
    worker: Option<Worker>,
    #[cfg(not(target_arch = "wasm32"))]
    next_id: u64,
    request_document: Option<photocraft_doc::DocId>,
    expected_revision: Option<u64>,
}
impl Drop for Generation {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Generation {
    pub fn cancel(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Relaxed);
        }
    }
    pub fn cancel_pending(&self) {
        if let Some(worker) = &self.worker {
            worker.cancel_pending.store(true, Ordering::Relaxed);
        }
    }
    pub fn tick(&mut self) {
        let mut done = false;
        if let Some(worker) = &self.worker {
            loop {
                match worker.receiver.try_recv() {
                    Ok(Event::Message(message, item)) => {
                        self.status.message = message;
                        self.status.remote_item = item;
                    }
                    Ok(Event::Candidate(mut candidate)) => {
                        if worker.cancel.load(Ordering::Relaxed) {
                            continue;
                        }
                        if self.request_document == Some(candidate.placement.document) {
                            candidate.placement.revision = self.expected_revision.unwrap_or(candidate.placement.revision);
                        }
                        self.candidates.push(*candidate);
                        self.status.completed += 1;
                    }
                    Ok(Event::Finished(result)) => {
                        self.status.message = match result {
                            Ok(()) => {
                                if self.status.total == 0 {
                                    self.status.message.clone()
                                } else {
                                    "Finished; accept or discard each candidate".into()
                                }
                            }
                            Err(e) => e.to_string(),
                        };
                        done = true;
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.status.message = "AI worker stopped unexpectedly; check the InvokeAI queue before retrying".into();
                        done = true;
                        break;
                    }
                }
            }
        }
        if done {
            self.worker = None;
            self.status.running = false;
            self.status.remote_item = None;
        }
    }
    /// Persisted document IDs can be reused after closing/reopening: invalidate that request.
    pub fn document_closed(&mut self, document: photocraft_doc::DocId) {
        for candidate in &mut self.candidates {
            if candidate.placement.document == document {
                candidate.placement.document_closed = true;
            }
        }
        if self.request_document == Some(document) && self.status.running && self.status.total > 0 {
            self.cancel();
        }
    }
    /// Each own acceptance advances the common request revision, including results still arriving.
    pub fn accepted_revision(&mut self, document: photocraft_doc::DocId, revision: u64) {
        if self.request_document == Some(document) {
            self.expected_revision = Some(revision);
        }
        for candidate in &mut self.candidates {
            if candidate.placement.document == document {
                candidate.placement.revision = revision;
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect(&mut self, settings: Settings) -> AiResult<()> {
        if self.worker.is_some() {
            return Err(AiError::Invalid("an AI operation is already running".into()));
        }
        settings.validate()?;
        let token = self.token.clone();
        self.spawn("Testing connection", 0, move |send, _cancel, _pending| {
            let mut client = super::invoke::InvokeClient::new(&settings, token)?;
            let message = client.health()?;
            send.send(Event::Message(message, None)).map_err(|_| AiError::Cancelled)?;
            Ok(())
        })
    }
    #[cfg(target_arch = "wasm32")]
    pub fn connect(&mut self, _settings: Settings) -> AiResult<()> {
        Err(AiError::Unsupported)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(
        &mut self,
        doc: Arc<photocraft_doc::Document>,
        revision: u64,
        active: Option<photocraft_doc::LayerId>,
        request: Request,
        settings: Settings,
    ) -> AiResult<()> {
        if self.worker.is_some() || !self.candidates.is_empty() {
            return Err(AiError::Invalid("finish the running operation and accept or discard existing candidates first".into()));
        }
        settings.validate()?;
        let workflow = settings
            .workflows
            .get(settings.selected_workflow)
            .cloned()
            .ok_or_else(|| AiError::Invalid("import and select an executable workflow template first".into()))?;
        request.validate(&workflow)?;
        let count = request.count.unwrap_or(1);
        let token = self.token.clone();
        self.next_id = self.next_id.checked_add(u64::from(count)).ok_or_else(|| AiError::Invalid("request identifier exhausted; restart PhotoCraft".into()))?;
        let id = self.next_id - u64::from(count) + 1;
        self.request_document = Some(doc.id);
        self.expected_revision = Some(revision);
        self.spawn("Preparing request", count, move |send, cancel, pending| {
            let prepared = super::images::prepare(&doc, revision, active, &request, &workflow, settings.context_padding)?;
            // The crop and placement own everything needed for inference. Release source tiles
            // before network waits so later editor writes do not retain a whole old document.
            drop(doc);
            let mut backend = super::invoke::InvokeClient::new(&settings, token)?;
            backend.health()?;
            generate_sequence(
                &mut backend,
                prepared,
                &workflow,
                &request,
                id,
                settings.job_timeout_secs,
                &cancel,
                &pending,
                |message, item| {
                    let _ = send.send(Event::Message(message, item));
                },
                |candidate| send.send(Event::Candidate(Box::new(candidate))).map_err(|_| AiError::Cancelled),
            )
        })
    }
    #[cfg(target_arch = "wasm32")]
    pub fn start(
        &mut self,
        _doc: Arc<photocraft_doc::Document>,
        _revision: u64,
        _active: Option<photocraft_doc::LayerId>,
        _request: Request,
        _settings: Settings,
    ) -> AiResult<()> {
        Err(AiError::Unsupported)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn spawn(
        &mut self,
        label: &str,
        total: u32,
        run: impl FnOnce(mpsc::Sender<Event>, Arc<AtomicBool>, Arc<AtomicBool>) -> AiResult<()> + Send + 'static,
    ) -> AiResult<()> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let cancel_pending = Arc::new(AtomicBool::new(false));
        let pending = cancel_pending.clone();
        std::thread::Builder::new()
            .name("photocraft-ai".into())
            .spawn(move || {
                let result = run(sender.clone(), flag, pending);
                let _ = sender.send(Event::Finished(result));
            })
            .map_err(|e| AiError::Backend(format!("cannot start AI worker: {e}")))?;
        self.status = Status { running: true, message: label.into(), total, ..Default::default() };
        self.worker = Some(Worker { receiver, cancel, cancel_pending });
        Ok(())
    }
}

/// Upload context once and run exactly the requested number, sequentially. Deliver as each completes.
#[allow(clippy::too_many_arguments)]
pub fn generate_sequence(
    backend: &mut dyn Backend,
    mut prepared: Prepared,
    workflow: &Workflow,
    request: &Request,
    first_id: u64,
    timeout_secs: u64,
    cancel: &AtomicBool,
    pending: &AtomicBool,
    mut progress: impl FnMut(String, Option<u64>),
    mut emit: impl FnMut(Candidate) -> AiResult<()>,
) -> AiResult<()> {
    let count = request.count.unwrap_or(1);
    request.validate(workflow)?;
    if u64::from(prepared.width) * u64::from(prepared.height) * u64::from(count) > 67_108_864 {
        return Err(AiError::Invalid("candidate queue exceeds 64 MP total; reduce resolution or count".into()));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(AiError::Cancelled);
    }
    if pending.load(Ordering::Relaxed) {
        return Err(AiError::PendingCancelled);
    }
    backend.validate_graph(workflow)?;
    let mut images = BTreeMap::new();
    if let Some(png) = prepared.reference.take() {
        images.insert("reference".into(), json!({"image_name":backend.upload(png,false)?}));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(AiError::Cancelled);
    }
    if let Some(png) = prepared.mask.take() {
        images.insert("mask".into(), json!({"image_name":backend.upload(png,true)?}));
    }
    let workflow = Workflow { graph: workflow.bind(&images)?, ..workflow.clone() };
    for n in 0..count {
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::Cancelled);
        }
        if pending.load(Ordering::Relaxed) {
            return Err(AiError::PendingCancelled);
        }
        let id = first_id.checked_add(u64::from(n)).ok_or_else(|| AiError::Invalid("candidate identifier overflow".into()))?;
        let mut variation = request.clone();
        variation.seed = request.seed.map(|v| v.wrapping_add(n));
        let candidate = generate(backend, prepared.clone(), &workflow, &variation, id, timeout_secs, cancel, |message, item| {
            progress(format!("{}/{}: {message}", n + 1, count), item)
        })?;
        if cancel.load(Ordering::Relaxed) {
            return Err(AiError::Cancelled);
        }
        emit(candidate)?;
    }
    Ok(())
}

/// One backend item. Cancellation is checked before every submission, poll and result delivery.
#[allow(clippy::too_many_arguments)] // Explicit immutable worker inputs; no session or document write access.
pub fn generate(
    backend: &mut dyn Backend,
    prepared: Prepared,
    workflow: &Workflow,
    request: &Request,
    id: u64,
    timeout_secs: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String, Option<u64>),
) -> AiResult<Candidate> {
    let check = || if cancel.load(Ordering::Relaxed) { Err(AiError::Cancelled) } else { Ok(()) };
    check()?;
    backend.validate_graph(workflow)?;
    let seed = workflow.bindings.contains_key("seed").then(|| request.seed.unwrap_or_else(random_seed));
    let mut values =
        BTreeMap::from([("prompt".into(), json!(request.prompt)), ("width".into(), json!(prepared.width)), ("height".into(), json!(prepared.height))]);
    if let Some(seed) = seed {
        values.insert("seed".into(), json!(seed));
    }
    for (key, value) in
        [("steps", request.steps.map(|v| json!(v))), ("guidance", request.guidance.map(|v| json!(v))), ("strength", request.strength.map(|v| json!(v)))]
    {
        if let Some(value) = value {
            values.insert(key.into(), value);
        }
    }
    if let Some(png) = prepared.reference {
        progress("Uploading cropped reference".into(), None);
        values.insert("reference".into(), json!({"image_name": backend.upload(png, false)?}));
    }
    check()?;
    if let Some(png) = prepared.mask {
        values.insert("mask".into(), json!({"image_name": backend.upload(png, true)?}));
    }
    check()?;
    let graph = workflow.bind(&values)?;
    let bound = Workflow { graph: graph.clone(), ..workflow.clone() };
    backend.validate_graph(&bound)?;
    progress("Submitting one generation".into(), None);
    let job = backend.submit(graph.clone())?;
    let started = Instant::now();
    let result = (|| {
        loop {
            check()?;
            if started.elapsed() >= Duration::from_secs(timeout_secs) {
                return Err(AiError::Timeout);
            }
            match backend.poll(job, &workflow.output_node)? {
                JobStatus::Complete(name) => {
                    progress("Downloading result".into(), Some(job));
                    let image = super::images::decode_result(&backend.image(&name)?)?;
                    check()?;
                    if image.dimensions() != (prepared.width, prepared.height) {
                        return Err(AiError::Backend(format!(
                            "result is {}×{}, expected {}×{}; fix width/height bindings",
                            image.width(),
                            image.height(),
                            prepared.width,
                            prepared.height
                        )));
                    }
                    return Ok(Candidate {
                        id,
                        placement: prepared.placement,
                        image,
                        metadata: json!({"prompt": request.prompt, "seed": seed, "workflow": workflow.name, "request": request, "parameters": values, "graph": graph, "remoteItem": job, "imageName": name}),
                    });
                }
                JobStatus::Failed(reason) => return Err(AiError::Backend(format!("item {job}: {reason}"))),
                JobStatus::Cancelled => return Err(AiError::Cancelled),
                JobStatus::Pending => progress("Waiting in InvokeAI queue".into(), Some(job)),
                JobStatus::Running { completed_nodes, total_nodes } => {
                    progress(format!("Generating: {completed_nodes}/{total_nodes} graph nodes completed"), Some(job))
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            for _ in 0..10 {
                check()?;
                std::thread::sleep(Duration::from_millis(100));
            }
            #[cfg(target_arch = "wasm32")]
            return Err(AiError::Unsupported);
        }
    })();
    if result.is_err()
        && let Err(error) = backend.cancel(job)
    {
        return Err(AiError::Backend(format!(
            "{}; cancelling item {job} failed: {error}. Stop this item in InvokeAI",
            result.as_ref().err().map_or_else(String::new, |e| e.to_string())
        )));
    }
    result
}
fn random_seed() -> u32 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |v| v.as_nanos() as u64);
    let hash = blake3::hash(&time.wrapping_add(n).to_le_bytes());
    let mut bytes = [0; 4];
    if let Some(source) = hash.as_bytes().get(..4) {
        bytes.copy_from_slice(source);
    }
    u32::from_le_bytes(bytes)
}

#[cfg(test)]
#[path = "queue_runtime_tests.rs"]
mod runtime_tests;
