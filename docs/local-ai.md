# Local AI generation in PhotoCraft

PhotoCraft uses a separately running local InvokeAI server. No model weights, model downloads,
telemetry or hosted generation services are included. The default URL is `http://127.0.0.1:9090`.
The HTTP contract was inspected against InvokeAI **6.14.2** at `/openapi.json` and `/docs` on
2026-10-08. Each connection loads the installed OpenAPI specification again. API compatibility
is checked at runtime; model-specific graph correctness remains the workflow author's responsibility.

## Executable workflow templates

The queue accepts executable graphs, rather than saved graphical workflow records. Export the
actual executable graph (for example, the `batch.graph` in a successful InvokeAI enqueue request).
Keep its existing model identifier objects and node types. Wrap it in this PhotoCraft template:

```json
{
  "name": "My local workflow",
  "graph": {"id": "your-graph-id", "nodes": {}, "edges": []},
  "bindings": {
    "prompt": ["/nodes/YOUR_PROMPT_NODE/prompt"],
    "seed": ["/nodes/YOUR_SEED_NODE/seed"],
    "width": ["/nodes/YOUR_SIZE_NODE/width"],
    "height": ["/nodes/YOUR_SIZE_NODE/height"]
  },
  "outputNode": "YOUR_IMAGE_OUTPUT_NODE",
  "modes": ["generate"],
  "maxWidth": 1024,
  "maxHeight": 1024,
  "dimensionMultiple": 8
}
```

The empty graph and `YOUR_*` strings above are explanatory placeholders, not an executable
workflow. Populate them from a working graph for your installed models. Each binding is a JSON
pointer to an existing direct-input field. One parameter may target several fields. Escape `~`
and `/` in node IDs as `~0` and `~1`. A field driven by an incoming edge cannot be bound directly.
Only mapped controls are exposed. Optional bindings: `steps`, `guidance`, `strength`, `reference`
and `mask`. Reference and mask bindings replace an entire image field with
`{"image_name":"<uploaded image>"}`. Node and model identifiers are never synthesized.

An executable graph must yield exactly one image at `outputNode`. Iterators, collectors, nested
graphs and saved-workflow calls are deliberately unsupported; they could silently multiply work.
All other outputs are marked intermediate. PhotoCraft never resumes or clears the server's shared
queue, and never retries submission automatically. If submission times out after reaching the
server, inspect InvokeAI's queue before submitting again.

FLUX.2 requires a tested local FLUX.2 executable graph. The inspected server had FLUX.2 models,
but no compatible saved FLUX.2 edit graph. No bundled FLUX.2 generation graph is claimed here.
Consult the installed `/docs`, rather than substituting node names from another model or release.

## Engine integration

The integration lives in new `crates/engine/src/ai/` modules and `ai_cmds.rs`; it uses the existing
session, command registry, preference storage and history APIs. Background workers only produce
temporary candidates. Accepting a candidate is an independent history step; discard leaves the
document unchanged. Results are normal pixel layers. Private `pcAI` additional-info blocks retain
prompt/seed/workflow/parameters without changing the document or native-format schema.

The commands are available to the CLI, desktop control channel and MCP:
`ai.configure`, `ai.connect`, `ai.generate`, `ai.status`, `ai.cancel`, `ai.accept`, `ai.discard`.
Configuration is saved in the existing preferences under `ai`. Bearer tokens are session-only and
must be entered again after restart; they are excluded from persisted preferences and metadata.

Generated RGB PNG/JPEG results are validated with allocation and pixel limits. Untagged model
results are interpreted as sRGB; tagged RGB results use their ICC profile. Pixels are converted
through PhotoCraft's colour management to the document's profile and native 8/16/32-bit depth.
AI models still operate at their own precision; this does not recover lost HDR detail.

## Verification

Normal tests use generated images and mock backends/HTTP servers; no GPU or InvokeAI is required:

```sh
cargo test -p photocraft-engine ai
cargo test -p photocraft-engine ai::invoke_tests::live_invoke_health -- --ignored --nocapture
```

The second command is an opt-in, read-only connection check. Set `PHOTOCRAFT_INVOKE_URL` and,
when needed, `PHOTOCRAFT_INVOKE_TOKEN` in your environment. No inference runs in this check.

## Necessary edits to existing files

- `crates/engine/Cargo.toml` and `Cargo.lock`: native-only HTTP/TLS transport dependency.
- `crates/engine/src/lib.rs`: module registration and independent session-owned AI queue.
- `crates/engine/src/commands.rs`: one command registry extension.
- `crates/engine/src/prefs.rs`: one defaulted persistent AI settings field.

No document types, image formats, compositor or history infrastructure are forked.

## Selection-aware editing and maskless fill

Reference source defaults to the active layer; select `mergedVisible` to composite visible layers
without flattening the document. An active selection always restricts the reference to its bounds
plus the configured context padding, clamped to the canvas. Empty/off-canvas selections and an
active source with no visible selected pixels return actionable errors. With no selection, Edit
uses the full chosen source; Generate uses no source image.

The original selection coverage is retained as a floating-point editable layer mask. Irregular
edges, antialiasing and feathered opacity are preserved. The reference travels as colour-managed
sRGB RGBA8 PNG; original document pixels/depth are untouched. Requests are limited to 64 MP crops
and 16 MP model images. Workflow dimension constraints resize only the cropped request, and the
returned image maps back to the original rectangle. Explicit nonzero width/height override model
resolution; zero/zero in editing modes derives it from the crop. Padding never expands the request
to the whole canvas unless the padded selection actually reaches those boundaries.

Maskless Fill uses a reference-edit workflow supporting `masklessFill`; it attaches the saved
selection as a mask after generation. FLUX.2 may change every pixel in the reference crop. Unchanged
visible pixels are preserved by PhotoCraft's original layers and the generated layer's mask.
Accept with `maskEnabled:false` to inspect the whole candidate crop, then enable and paint its mask
using the existing Layers panel and painting tools. Undo/redo applies to each acceptance and mask
edit. If the document revision changed, acceptance requires an explicit `allowStale:true` review;
changing canvas dimensions, mode, depth or ICC profile blocks acceptance entirely.
