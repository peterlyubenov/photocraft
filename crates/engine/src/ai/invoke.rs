//! InvokeAI 6.14.2 HTTP protocol, verified against the local /openapi.json and /docs.
use std::io::Read;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, multipart};
use serde_json::{Value, json};

use super::{AiError, AiResult, Backend, JobStatus, Settings, workflow::Workflow};

const MAX_RESPONSE: u64 = 64 << 20;
pub struct InvokeClient {
    client: Client,
    base: String,
    token: String,
    schema: Option<Value>,
    can_cancel: bool,
}
impl InvokeClient {
    pub fn new(settings: &Settings, token: String) -> AiResult<Self> {
        settings.validate()?;
        let url = reqwest::Url::parse(&settings.server_url).map_err(|_| AiError::Invalid("invalid InvokeAI URL".into()))?;
        if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(AiError::Invalid("URL needs a host and must not contain credentials, query or fragment; use a bearer token".into()));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(settings.request_timeout_secs))
            .user_agent("Photocraft-dev")
            .build()
            .map_err(network)?;
        Ok(Self { client, base: settings.server_url.trim_end_matches('/').into(), token, schema: None, can_cancel: false })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let request = self.client.request(method, format!("{}{path}", self.base));
        if self.token.is_empty() { request } else { request.bearer_auth(&self.token) }
    }
    fn bytes(&self, request: RequestBuilder) -> AiResult<Vec<u8>> {
        let response = request.send().map_err(network)?;
        let status = response.status();
        if !status.is_success() {
            let hint = match status.as_u16() {
                401 | 403 => "authentication failed; enter a bearer token for this InvokeAI server",
                404 => "endpoint or image missing; test the connection and check the installed API version",
                422 => "workflow rejected; check executable graph fields and model identifiers against /docs",
                429 => "queue capacity reached; wait before explicitly submitting again",
                _ => "check the InvokeAI server log",
            };
            // Server bodies may contain secrets or source images: do not echo them into logs.
            return Err(AiError::Backend(format!("HTTP {status}: {hint}")));
        }
        if response.content_length().is_some_and(|n| n > MAX_RESPONSE) {
            return Err(AiError::Backend("response exceeds 64 MB limit".into()));
        }
        let mut bytes = Vec::new();
        response.take(MAX_RESPONSE + 1).read_to_end(&mut bytes).map_err(|e| AiError::Backend(format!("reading response: {e}")))?;
        if bytes.len() as u64 > MAX_RESPONSE {
            return Err(AiError::Backend("response exceeds 64 MB limit".into()));
        }
        Ok(bytes)
    }
    fn json(&self, request: RequestBuilder) -> AiResult<Value> {
        serde_json::from_slice(&self.bytes(request)?).map_err(|e| AiError::Backend(format!("invalid API JSON: {e}")))
    }
    fn image_path(&self, name: &str, suffix: &str) -> AiResult<String> {
        if name.is_empty() || name.len() > 256 || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
            return Err(AiError::Backend("server returned an invalid image name".into()));
        }
        Ok(format!("/api/v1/images/i/{name}{suffix}"))
    }
}
fn network(error: reqwest::Error) -> AiError {
    if error.is_timeout() {
        AiError::Backend("HTTP request timed out; check the server and network. Submissions are never retried automatically".into())
    } else {
        AiError::Backend(
            "HTTP connection failed; start InvokeAI and check the server URL. An ambiguous submission may exist in InvokeAI's queue; check before resubmitting"
                .into(),
        )
    }
}
impl Backend for InvokeClient {
    fn health(&mut self) -> AiResult<String> {
        let schema = self.json(self.request(reqwest::Method::GET, "/openapi.json"))?;
        let paths = schema.get("paths").ok_or_else(|| AiError::Backend("OpenAPI has no paths".into()))?;
        for (path, method) in [
            ("/api/v1/queue/{queue_id}/enqueue_batch", "post"),
            ("/api/v1/queue/{queue_id}/i/{item_id}", "get"),
            ("/api/v1/images/upload", "post"),
            ("/api/v1/images/i/{image_name}/full", "get"),
        ] {
            if paths.get(path).and_then(|v| v.get(method)).is_none() {
                return Err(AiError::Backend(format!("unsupported installed API: missing {method} {path}")));
            }
        }
        self.can_cancel = paths.get("/api/v1/queue/{queue_id}/i/{item_id}/cancel").and_then(|v| v.get("put")).is_some();
        self.schema = Some(schema);
        let version = self.json(self.request(reqwest::Method::GET, "/api/v1/app/version"))?;
        // Version is public; queue status checks credentials without changing processor state.
        self.json(self.request(reqwest::Method::GET, "/api/v1/queue/default/status"))?;
        Ok(format!(
            "InvokeAI {} connected{}",
            version.get("version").and_then(Value::as_str).unwrap_or("unknown"),
            if self.can_cancel { " (cancellation available)" } else { " (server cancellation unavailable)" }
        ))
    }
    fn validate_graph(&self, workflow: &Workflow) -> AiResult<()> {
        workflow.validate()?;
        let schema = self.schema.as_ref().ok_or_else(|| AiError::Backend("test the connection before validating a graph".into()))?;
        let types = schema
            .pointer("/components/schemas/Graph/properties/nodes/additionalProperties/oneOf")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::Backend("installed API lacks graph node alternatives".into()))?;
        let nodes = workflow.graph.get("nodes").and_then(Value::as_object).ok_or_else(|| AiError::Invalid("invalid graph".into()))?;
        for (id, node) in nodes {
            let kind = node.get("type").and_then(Value::as_str).unwrap_or("");
            let node_schema = types
                .iter()
                .filter_map(|v| v.get("$ref").and_then(Value::as_str))
                .filter_map(|v| schema.pointer(v.trim_start_matches('#')))
                .find(|v| v.pointer("/properties/type/const").and_then(Value::as_str) == Some(kind))
                .ok_or_else(|| AiError::Invalid(format!("node type {kind} is not in this server's OpenAPI")))?;
            // Reject hosted generation nodes: this integration is for local inference only.
            if node_schema.get("category").and_then(Value::as_str) == Some("api")
                || ["openai", "alibaba", "bria", "bfl", "remote_api"].iter().any(|s| kind.contains(s))
            {
                return Err(AiError::Invalid("cloud invocation nodes are not supported by local generation".into()));
            }
            let properties = node_schema.get("properties").and_then(Value::as_object).ok_or_else(|| AiError::Invalid("node schema lacks properties".into()))?;
            if let Some(object) = node.as_object() {
                for (key, value) in object {
                    let field = properties.get(key).ok_or_else(|| AiError::Invalid(format!("unknown field {id}.{key}")))?;
                    validate_value(value, field, schema, 0).map_err(|e| AiError::Invalid(format!("{id}.{key}: {e}")))?;
                }
            }
            if id == &workflow.output_node {
                let output =
                    node_schema.get("output").and_then(|v| v.get("$ref")).and_then(Value::as_str).and_then(|v| schema.pointer(v.trim_start_matches('#')));
                if output.and_then(|v| v.get("properties")).and_then(|v| v.get("image")).is_none() {
                    return Err(AiError::Invalid("outputNode must return an image field".into()));
                }
            }
        }
        Ok(())
    }
    fn upload(&mut self, png: Vec<u8>, mask: bool) -> AiResult<String> {
        let part = multipart::Part::bytes(png).file_name("photocraft.png").mime_str("image/png").map_err(network)?;
        let result = self.json(
            self.request(reqwest::Method::POST, "/api/v1/images/upload")
                .query(&[("image_category", if mask { "mask" } else { "user" }), ("is_intermediate", "true"), ("crop_visible", "false")])
                .multipart(multipart::Form::new().part("file", part)),
        )?;
        let name = result.get("image_name").and_then(Value::as_str).ok_or_else(|| AiError::Backend("upload response lacks image_name".into()))?;
        self.image_path(name, "")?;
        Ok(name.into())
    }
    fn submit(&mut self, graph: Value) -> AiResult<u64> {
        let result = self.json(
            self.request(reqwest::Method::POST, "/api/v1/queue/default/enqueue_batch").json(&json!({"batch": {"graph": graph, "runs": 1}, "prepend": false})),
        )?;
        let ids = result
            .get("item_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| AiError::Backend("submission lacks item_ids; inspect InvokeAI's queue before retrying".into()))?;
        if ids.len() != 1 || result.get("enqueued").and_then(Value::as_u64) != Some(1) {
            return Err(AiError::Backend("server did not enqueue exactly one item; inspect InvokeAI's queue before retrying".into()));
        }
        ids.first().and_then(Value::as_u64).ok_or_else(|| AiError::Backend("invalid queue item id".into()))
    }
    fn poll(&mut self, job: u64, output_node: &str) -> AiResult<JobStatus> {
        let item = self.json(self.request(reqwest::Method::GET, &format!("/api/v1/queue/default/i/{job}")))?;
        Ok(match item.get("status").and_then(Value::as_str) {
            Some("pending" | "waiting") => JobStatus::Pending,
            Some("in_progress") => JobStatus::Running {
                completed_nodes: item.pointer("/session/executed").and_then(Value::as_array).map_or(0, Vec::len),
                total_nodes: item.pointer("/session/graph/nodes").and_then(Value::as_object).map_or(0, |v| v.len()),
            },
            Some("completed") => {
                let results = item.pointer("/session/results").and_then(Value::as_object);
                let direct = results.and_then(|v| v.get(output_node));
                // InvokeAI may expand source nodes to prepared execution node IDs.
                let prepared = item
                    .pointer("/session/source_prepared_mapping")
                    .and_then(|v| v.get(output_node))
                    .and_then(Value::as_array)
                    .and_then(|ids| if ids.len() == 1 { ids.first().and_then(Value::as_str) } else { None })
                    .and_then(|id| results.and_then(|v| v.get(id)));
                let name = direct
                    .or(prepared)
                    .and_then(|v| v.pointer("/image/image_name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| AiError::Backend("completed graph has no single image for outputNode; check the workflow mapping".into()))?;
                self.image_path(name, "")?;
                JobStatus::Complete(name.into())
            }
            Some("failed") => JobStatus::Failed(
                item.get("error_type").and_then(Value::as_str).unwrap_or("workflow failed; inspect InvokeAI's server log").chars().take(256).collect(),
            ),
            Some("canceled") => JobStatus::Cancelled,
            _ => return Err(AiError::Backend("unknown queue item status".into())),
        })
    }
    fn cancel(&mut self, job: u64) -> AiResult<()> {
        if !self.can_cancel {
            return Err(AiError::Backend("server does not support cancellation; stop this item in InvokeAI".into()));
        }
        self.bytes(self.request(reqwest::Method::PUT, &format!("/api/v1/queue/default/i/{job}/cancel")))?;
        Ok(())
    }
    fn image(&mut self, name: &str) -> AiResult<Vec<u8>> {
        self.bytes(self.request(reqwest::Method::GET, &self.image_path(name, "/full")?))
    }
}

/// Small bounded validator for direct input values; InvokeAI performs full graph validation.
fn validate_value(value: &Value, rule: &Value, root: &Value, depth: u32) -> Result<(), String> {
    if depth > 24 {
        return Err("schema nesting exceeds limit".into());
    }
    if let Some(reference) = rule.get("$ref").and_then(Value::as_str) {
        return validate_value(value, root.pointer(reference.trim_start_matches('#')).ok_or("missing referenced schema")?, root, depth + 1);
    }
    if let Some(alternatives) = rule.get("anyOf").or_else(|| rule.get("oneOf")).and_then(Value::as_array) {
        if alternatives.iter().any(|v| validate_value(value, v, root, depth + 1).is_ok()) {
            return Ok(());
        }
        return Err("value does not match the server's field schema".into());
    }
    if rule.get("const").is_some_and(|v| v != value) || rule.get("enum").and_then(Value::as_array).is_some_and(|v| !v.contains(value)) {
        return Err("value is not a supported choice".into());
    }
    let valid = match rule.get("type").and_then(Value::as_str) {
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("number") => value.is_number(),
        Some("string") => value.is_string(),
        Some("object") => value.is_object(),
        Some("array") => value.is_array(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        _ => true,
    };
    if !valid {
        return Err("wrong value type".into());
    }
    if let Some(n) = value.as_f64()
        && (rule.get("minimum").and_then(Value::as_f64).is_some_and(|v| n < v) || rule.get("maximum").and_then(Value::as_f64).is_some_and(|v| n > v))
    {
        return Err("value exceeds the server's supported range".into());
    }
    if let Some(properties) = rule.get("properties").and_then(Value::as_object) {
        if let Some(required) = rule.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if value.get(key).is_none() {
                    return Err(format!("missing required {key}"));
                }
            }
        }
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                if let Some(field) = properties.get(key) {
                    validate_value(value, field, root, depth + 1)?;
                }
            }
        }
    }
    if let (Some(array), Some(items)) = (value.as_array(), rule.get("items")) {
        for value in array {
            validate_value(value, items, root, depth + 1)?;
        }
    }
    Ok(())
}
