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

The result panel can be reviewed offscreen using synthetic pixels:

```sh
cargo run -p photocraft-ui-egui --example ai_snapshot -- /tmp/ai-results.png --masked
cargo run --release -p photocraft-engine --example ai_crop_bench
```

The snapshot also accepts `--light`, `--small` (800×600) and `--running` (synthetic queue). The benchmark compares a full
24 MP reference with a 512×512 selection plus 32 pixels of context. Neither invokes a model.

Verification on 2026-10-08:

- Existing stable Rust/Cargo 1.99.0 and installed wasm target were used. Native X11/Wayland
  build dependencies were available; no global tools or model weights were installed.
- Baseline engine library: 771 passed, 11 ignored. Final engine library: 793 passed, 13 ignored;
  UI library: 823 passed, 3 ignored. There are 26 new normal regression tests and two opt-in live tests.
- Formatting, workspace clippy with warnings denied, layering, wasm, parity (627/627), scorecard
  and the ignored adversarial-command `panic_hunt` passed.
- Plain workspace testing found two pre-existing failures, reproduced on original revision
  `4526727`: `workspace::tests::registry_filesystem_path_params_are_classified` (four existing
  vector-path commands) and `i18n_coverage::tests::source_key_set_is_covered_by_complete_catalogs`
  (missing Czech `{n} layer|{n} layers`). Workspace tests, including doctests, passed with only
  those two tests skipped. Scorecard staleness from the new preference was regenerated and fixed.
- Read-only health passed against InvokeAI 6.14.2. Real generation was **not run**.
- Dark/light snapshots at 1440×900 and 800×600 were inspected, including masks, results and queue
  controls. UI tests click Accept/Discard and verify history.
- One release preparation measurement: full 6000×4000 reference 3527 ms (model 1024×680);
  512×512 selection plus context 64 ms (crop/model 576×576). This measures export, not inference.

The sibling `../craftrules` checkout was absent; its README could not be read. Repository
instructions and the available PhotoCraft documentation were followed.

All development build/cache files were kept under `/tmp` after the storage shortage. To reuse
this session's cache when launching on this machine:

```sh
CARGO_HOME=/tmp/photocraft-cargo CARGO_TARGET_DIR=/tmp/photocraft-ai-target \
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo run -p photocraft
```

These temporary directories can disappear after reboot; ordinary `cargo run -p photocraft` uses
your normal Cargo cache and project target directory.

## Added files

- `crates/engine/src/ai/{mod,invoke,workflow,images,queue}.rs`: backend interface, installed-API
  transport, explicit graph bindings, colour-managed crops/masks and independent worker queue.
- `crates/engine/src/ai/{invoke_tests,images_tests,queue_tests,queue_runtime_tests,smoke_tests}.rs`:
  synthetic/mock tests and opt-in live checks.
- `crates/engine/src/ai_cmds.rs` and `ai_cmds/tests.rs`: command registration/implementations and
  document/history regression tests.
- `crates/ui-egui/src/ai_ui.rs` and `ai_ui/tests.rs`: native panel, cached previews and UI tests.
- `crates/engine/examples/ai_crop_bench.rs` and `crates/ui-egui/examples/ai_snapshot.rs`:
  reproducible synthetic performance/visual checks.
- `docs/local-ai.md`: configuration, usage, verification and integration boundaries.

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

For reverse inpainting without a selection, use Edit to accept the full candidate, then choose
**Layer → Layer Mask → Hide All**. Click its mask thumbnail and paint white to reveal only the
generated pixels you want to keep; the original remains underneath. With a saved selection mask,
accept with its mask disabled to inspect the crop first, then enable and refine that mask.

## Stable Diffusion inpainting

Use a tested local Stable Diffusion inpainting executable graph with `reference` and `mask`
bindings, `"modes":["inpaint"]`, and an explicit `maskSemantics`:

- `whiteRepaints`: selection coverage is exported as grayscale white=repaint, black=retain.
- `blackRepaints`: inverted grayscale coverage, black=repaint, white=retain.
- `transparentRepaints`: RGBA white with alpha=1−coverage, transparent=repaint.

These are template declarations, not guesses about a node's implementation. Verify the chosen
workflow's actual semantics in your installed InvokeAI `/docs` and a small known mask example.
Both reference and mask use the same crop and model resolution. After generation, the original
floating-point selection mask is attached independently, preserving feathered compositing even
when the backend mask is quantized/resized. Import a separate template for maskless FLUX.2 editing
and for true Stable Diffusion inpainting. No compatible SD inpainting model/graph was available
on the inspected local server, so GPU-level inpainting compatibility remains unverified.

## Sequential queue and variations

Count defaults to **1**, supports 1/2/4 and custom 1–16, and is never multiplied by graph batching.
Reference/mask uploads are reused across sequential jobs. Candidate storage is limited to 64 MP
total. A fixed seed increments for each variation; random mode chooses a fresh seed per item.
Seed behavior requires a seed binding. Results arrive individually and remain temporary until
accepted. Each acceptance is separately undoable. You may keep any subset and discard the rest.
Cancellation stops pending items and attempts to cancel only this request's running item, without
clearing someone else's queue. Unsupported/failed cancellation reports the item ID for manual
stopping in InvokeAI. Late cancelled results are ignored. Connection polling never resubmits work.
**Cancel remaining jobs** (`ai.cancel {"pendingOnly":true}`) lets the current submitted item finish
and retains its result, while dropping later variations. **Cancel running and pending jobs** also
attempts server cancellation of the current item.
Application shutdown requests cancellation; delivery is best effort if the process exits before
an HTTP call finishes. Check InvokeAI's queue after forced termination or an ambiguous timeout.

## Desktop use

Launch normally with `cargo run -p photocraft`. Open or create a document, then choose
**Window → AI Generation** (also available from a selection tool's context menu).

1. Expand **Connection and executable workflows**. Set the URL and optional session bearer token.
2. Paste the complete PhotoCraft template JSON, choose **Import template**, then **Save settings**.
3. Choose **Test connection**. A successful connection also validates the installed queue/image API.
4. Select a model/workflow template and one of its supported modes. Enter a prompt/instruction.
5. For editing/fill, choose active layer or merged visible, and make a selection before filling.
6. Choose count (default 1), configure supported advanced parameters, and press **Generate**.
7. Continue editing during inference. Preview each candidate with/without its saved mask, then
   accept it as an ordinary layer or discard it. Review any document-change warning explicitly.
8. Paint the accepted layer mask using the existing mask thumbnail/Brush workflow. You can accept
   with the mask disabled, inspect the candidate, and enable/refine it afterward.

To regenerate, resolve all temporary candidates, then choose **Regenerate with new seed**.
It uses the current panel's prompt, source/selection, count and workflow, with a new random seed.
The graph's model identifiers determine the model; use separate named templates for model choices.
No unsupported steps/guidance/strength/seed controls are shown. Closing a document invalidates its
candidates even if the same persisted ID is later reopened; running inference is cancelled.
Candidates already completed can still be inspected/discarded and recovered through InvokeAI.

The panel is native egui, modeless and driven through the existing commands. Form state is exposed
through `ui.inspect`/`ui.set`; tokens are excluded from serialized UI state. Queue state is available
through `ai.status`. The panel title/menu is localized; newly added explanatory form text currently
uses English. The wasm build reports the local backend unavailable and remains usable as an editor.

Additional existing-file integration edits:

- `crates/engine/src/float_cmds.rs`: preserve floating selections for AI queries/config/discard;
  generation and acceptance are disabled until the float is resolved.
- `crates/engine/src/lib.rs` close hook: invalidate/cancel the original document's request, preventing
  a closed/reopened persisted ID from admitting a stale candidate.
- `crates/ui-egui/src/lib.rs`: register the panel, thumbnail cache and per-frame poll/draw calls.
- `crates/ui-egui/src/state.rs`: one defaulted serializable panel form field.
- `crates/ui-egui/src/menus.rs`: one Window entry, routing, enablement and check state.
- `crates/ui-egui/src/canvas_tool_menu.rs`: one selection context entry using that same panel command.
- `crates/ui-egui/src/control.rs`: expose/patch the panel form through existing inspection/control;
  patches never submit a job. The context-menu test includes the intentional additional entry.
- `crates/ui-egui/src/i18n/*.tsv`: one panel title translation per shipped locale to preserve menu coverage.
- `docs/roadmap.md`: honest fork integration status; real inference remains unverified.
- `docs/scorecard.md`: regenerated preference count (135 → 136; unread count remains 57).

## Known boundaries

Real FLUX.2 editing and SD inpainting inference remain unverified until you supply working executable
templates. Server support is checked against OpenAPI, but PhotoCraft does not convert arbitrary
saved graphical workflows, manage models, modify InvokeAI's queue processor, provide batching, or
stream denoising-step percentages. Progress reports queue state and completed graph nodes. Templates
must bind prompt/width/height; optional controls depend on their explicit bindings. Crops/output are
resized within declared limits, so extreme aspect ratios or inaccurate workflow constraints may
require a better template. HTTP cancellation cannot interrupt a stuck socket before its timeout.
Generate without a selection keeps the canvas size; a larger generated layer can extend beyond
the canvas. Resize/create the canvas when you want to see that entire layer.

A separate, **expensive**, explicitly opt-in smoke test submits exactly one real job. Supply an
already tested template; it uses synthetic source pixels for Edit/Fill and requires no personal image:

```sh
PHOTOCRAFT_AI_SMOKE_GENERATE=1 \
PHOTOCRAFT_AI_SMOKE_WORKFLOW=/absolute/path/my-template.json \
PHOTOCRAFT_AI_SMOKE_MODE=generate \
cargo test -p photocraft-engine ai::smoke_tests::live_generation_smoke -- --ignored --nocapture
```

Modes are `generate`, `edit`, `masklessFill` or `inpaint`, supported by the supplied template.
Optional `PHOTOCRAFT_AI_SMOKE_PROMPT` changes the prompt. Normal tests never run this test. The
checked-in code includes no preconfigured FLUX.2 graph and never initiates expensive inference
as part of connection testing, rendering, retry handling or normal workspace tests.
