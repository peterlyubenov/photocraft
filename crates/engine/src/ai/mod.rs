//! Local generation. Transport and worker results never mutate an editor document.
pub mod images;
#[cfg(not(target_arch = "wasm32"))]
pub mod invoke;
pub mod queue;
pub mod workflow;

use serde::{Deserialize, Serialize};

pub type AiResult<T> = std::result::Result<T, AiError>;
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("AI configuration: {0}")]
    Invalid(String),
    #[error("InvokeAI: {0}")]
    Backend(String),
    #[error("AI generation cancelled")]
    Cancelled,
    #[error("AI generation timed out; check InvokeAI's queue and increase the job timeout")]
    Timeout,
    #[error("local InvokeAI is unavailable in the web build")]
    Unsupported,
}
impl From<AiError> for crate::EngineError {
    fn from(e: AiError) -> Self {
        Self::Other(e.to_string())
    }
}

pub const MAX_PIXELS: u64 = 16_777_216;
pub const MAX_COUNT: u32 = 16;
pub fn check_size(width: u32, height: u32) -> AiResult<()> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(AiError::Invalid("image dimensions must be 1–8192, at most 16 MP; select a smaller region".into()));
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    #[default]
    Generate,
    Edit,
    MasklessFill,
    Inpaint,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    #[default]
    ActiveLayer,
    MergedVisible,
}

/// Serialized by the existing preferences store. Authentication stays session-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub server_url: String,
    pub request_timeout_secs: u64,
    pub job_timeout_secs: u64,
    pub context_padding: u32,
    pub workflows: Vec<workflow::Workflow>,
    pub selected_workflow: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:9090".into(),
            request_timeout_secs: 15,
            job_timeout_secs: 1800,
            context_padding: 32,
            workflows: Vec::new(),
            selected_workflow: 0,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> AiResult<()> {
        if !(1..=120).contains(&self.request_timeout_secs)
            || !(1..=86400).contains(&self.job_timeout_secs)
            || self.context_padding > 2048
            || self.workflows.len() > 32
            || self.server_url.len() > 2048
        {
            return Err(AiError::Invalid("invalid timeouts, padding, URL or workflow count".into()));
        }
        if !self.server_url.starts_with("http://") && !self.server_url.starts_with("https://") {
            return Err(AiError::Invalid("server URL must use http:// or https://".into()));
        }
        for workflow in &self.workflows {
            workflow.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub prompt: String,
    pub mode: Mode,
    pub source: Source,
    pub width: u32,
    pub height: u32,
    pub seed: Option<u32>,
    pub steps: Option<u32>,
    pub guidance: Option<f32>,
    pub strength: Option<f32>,
    pub count: Option<u32>,
}
impl Request {
    pub fn validate(&self, workflow: &workflow::Workflow) -> AiResult<()> {
        if self.prompt.trim().is_empty() || self.prompt.len() > 16384 {
            return Err(AiError::Invalid("enter a prompt (at most 16384 bytes)".into()));
        }
        if !workflow.modes.contains(&self.mode) {
            return Err(AiError::Invalid("selected workflow does not support this mode".into()));
        }
        if !(1..=MAX_COUNT).contains(&self.count.unwrap_or(1)) {
            return Err(AiError::Invalid("generation count must be 1–16".into()));
        }
        for (key, set, valid) in [
            ("steps", self.steps.is_some(), self.steps.is_none_or(|n| (1..=500).contains(&n))),
            ("guidance", self.guidance.is_some(), self.guidance.is_none_or(|v| v.is_finite() && (0.0..=100.0).contains(&v))),
            ("strength", self.strength.is_some(), self.strength.is_none_or(|v| v.is_finite() && (0.0..=1.0).contains(&v))),
            ("seed", self.seed.is_some(), true),
        ] {
            if !valid || (set && !workflow.bindings.contains_key(key)) {
                return Err(AiError::Invalid(format!("unsupported or out-of-range {key}")));
            }
        }
        Ok(())
    }
}

/// Backend seam for future local backends and deterministic mock workers.
pub trait Backend: Send {
    fn health(&mut self) -> AiResult<String>;
    fn validate_graph(&self, workflow: &workflow::Workflow) -> AiResult<()>;
    fn upload(&mut self, png: Vec<u8>, mask: bool) -> AiResult<String>;
    fn submit(&mut self, graph: serde_json::Value) -> AiResult<u64>;
    fn poll(&mut self, job: u64, output_node: &str) -> AiResult<JobStatus>;
    fn cancel(&mut self, job: u64) -> AiResult<()>;
    fn image(&mut self, name: &str) -> AiResult<Vec<u8>>;
}
#[derive(Clone, Debug)]
pub enum JobStatus {
    Pending,
    Running { completed_nodes: usize, total_nodes: usize },
    Complete(String),
    Failed(String),
    Cancelled,
}

#[cfg(test)]
mod images_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod invoke_tests;
#[cfg(test)]
mod queue_tests;
