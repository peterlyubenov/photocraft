//! Explicit executable graphs: no model-specific node names are synthesized here.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AiError, AiResult, Mode, check_size};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Workflow {
    pub name: String,
    pub graph: Value,
    /// JSON pointers into the graph; every exposed parameter must have an explicit binding.
    pub bindings: BTreeMap<String, Vec<String>>,
    pub output_node: String,
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub mask_semantics: Option<MaskSemantics>,
    #[serde(default = "max_dimension")]
    pub max_width: u32,
    #[serde(default = "max_dimension")]
    pub max_height: u32,
    #[serde(default = "multiple")]
    pub dimension_multiple: u32,
}
fn max_dimension() -> u32 {
    2048
}
fn multiple() -> u32 {
    8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskSemantics {
    WhiteRepaints,
    BlackRepaints,
    TransparentRepaints,
}

impl Workflow {
    pub fn validate(&self) -> AiResult<()> {
        if self.name.is_empty() || self.name.len() > 256 {
            return Err(AiError::Invalid("workflow needs a name (at most 256 bytes)".into()));
        }
        check_size(self.max_width, self.max_height)?;
        if self.dimension_multiple == 0 || self.dimension_multiple > self.max_width.min(self.max_height) {
            return Err(AiError::Invalid("invalid workflow dimension multiple".into()));
        }
        let nodes = self
            .graph
            .get("nodes")
            .and_then(Value::as_object)
            .ok_or_else(|| AiError::Invalid("expected an executable graph with a nodes object; a saved UI workflow is not executable".into()))?;
        if nodes.is_empty() || nodes.len() > 256 {
            return Err(AiError::Invalid("graph needs 1–256 nodes".into()));
        }
        for (id, node) in nodes {
            if node.get("id").and_then(Value::as_str) != Some(id) || node.get("type").and_then(Value::as_str).is_none() {
                return Err(AiError::Invalid(format!("node {id} needs matching id and a type")));
            }
            if matches!(node.get("type").and_then(Value::as_str), Some("iterate" | "collect" | "graph" | "call_saved_workflow")) {
                return Err(AiError::Invalid("batch, nested and workflow-call nodes are unsupported: each request must produce one image".into()));
            }
        }
        if !nodes.contains_key(&self.output_node) {
            return Err(AiError::Invalid("outputNode must identify one image-producing node".into()));
        }
        let edges = self.graph.get("edges").and_then(Value::as_array).ok_or_else(|| AiError::Invalid("graph needs an edges array".into()))?;
        if edges.len() > 1024 {
            return Err(AiError::Invalid("graph has too many edges".into()));
        }
        for edge in edges {
            for end in ["source", "destination"] {
                let id = edge.get(end).and_then(|e| e.get("node_id")).and_then(Value::as_str).ok_or_else(|| AiError::Invalid("invalid graph edge".into()))?;
                if !nodes.contains_key(id) {
                    return Err(AiError::Invalid(format!("edge references missing node {id}")));
                }
            }
        }
        if self.modes.is_empty() || !self.bindings.contains_key("prompt") {
            return Err(AiError::Invalid("workflow needs modes and a prompt binding".into()));
        }
        if self.modes.iter().any(|m| *m != Mode::Generate) && !self.bindings.contains_key("reference") {
            return Err(AiError::Invalid("editing workflows need a reference binding".into()));
        }
        if self.modes.contains(&Mode::Inpaint) && (!self.bindings.contains_key("mask") || self.mask_semantics.is_none()) {
            return Err(AiError::Invalid("inpainting needs a mask binding and explicit maskSemantics".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for (parameter, pointers) in &self.bindings {
            if !["prompt", "seed", "width", "height", "steps", "guidance", "strength", "reference", "mask"].contains(&parameter.as_str())
                || pointers.is_empty()
                || pointers.len() > 64
            {
                return Err(AiError::Invalid(format!("unsupported binding {parameter}")));
            }
            for pointer in pointers {
                if !pointer.starts_with("/nodes/")
                    || pointer.ends_with("/id")
                    || pointer.ends_with("/type")
                    || !seen.insert(pointer)
                    || self.graph.pointer(pointer).is_none()
                {
                    return Err(AiError::Invalid(format!("invalid or duplicate graph pointer {pointer}")));
                }
                if edges.iter().any(|e| {
                    let destination = e.get("destination");
                    let id = destination.and_then(|v| v.get("node_id")).and_then(Value::as_str).unwrap_or("");
                    let field = destination.and_then(|v| v.get("field")).and_then(Value::as_str).unwrap_or("");
                    let escape = |s: &str| s.replace('~', "~0").replace('/', "~1");
                    pointer == &format!("/nodes/{}/{}", escape(id), escape(field))
                }) {
                    return Err(AiError::Invalid(format!("binding {pointer} is overridden by an incoming edge")));
                }
            }
        }
        Ok(())
    }

    pub fn dimensions(&self, width: u32, height: u32) -> AiResult<(u32, u32)> {
        check_size(width, height)?;
        let factor = (self.max_width as f64 / width as f64).min(self.max_height as f64 / height as f64).min(1.0);
        let fit = |n: u32, max: u32| {
            ((n as f64 * factor) as u32 / self.dimension_multiple * self.dimension_multiple)
                .max(self.dimension_multiple)
                .min(max / self.dimension_multiple * self.dimension_multiple)
        };
        Ok((fit(width, self.max_width), fit(height, self.max_height)))
    }

    pub fn bind(&self, values: &BTreeMap<String, Value>) -> AiResult<Value> {
        self.validate()?;
        let mut graph = self.graph.clone();
        for (key, pointers) in &self.bindings {
            if let Some(value) = values.get(key) {
                for pointer in pointers {
                    let slot = graph.pointer_mut(pointer).ok_or_else(|| AiError::Invalid(format!("missing binding {pointer}")))?;
                    *slot = value.clone();
                }
            }
        }
        if let Some(nodes) = graph.get_mut("nodes").and_then(Value::as_object_mut) {
            for (id, node) in nodes {
                if let Some(object) = node.as_object_mut() {
                    object.insert("is_intermediate".into(), json!(id != &self.output_node));
                }
            }
        }
        Ok(graph)
    }
}
