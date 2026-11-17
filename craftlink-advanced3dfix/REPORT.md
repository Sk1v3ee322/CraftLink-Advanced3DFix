# CraftLink + Layer Styles on 3D text — work report

**Date:** 17 November 2026
**Base versions:** FilmCraft 0.20.1, EffectCraft 0.24.1 (Rust)
**Author:** sk1v3e (with an AI agent)
**Status:** implemented, built; all tests these patches add pass. Two caveats are pre-existing and
documented: three environment-only test failures and two lints in FilmCraft (§V.4).

---

## 0. Summary for a busy reader

**Where to start, depending on who you are:**

| If you are… | Read, in this order |
| --- | --- |
| in a hurry | §0 (this page), then `MESSAGE-TO-DEVELOPER.txt` |
| applying the patches | `MESSAGE-TO-DEVELOPER.txt` → PART VI (apply commands) → PART V (what is in the archive) |
| reviewing the code | §0 → §V.1 (claim → file table) → PART I / PART II → the patch itself |
| an AI agent taking over the work | this report top to bottom, then `CRAFTLINK_ARCHITECTURE.md` (§0–§5 are the design, §6 the file/function/command tables), then the patches |
| an AI agent that must trust nothing | PART IV (test results) + §V.2 and §V.4 (exact commands and their output) |

Everything factual in this report is tied to a file or a command you can re-run; §V.1 is the index of
those pairs. The two Russian files are translations of `REPORT.md` and `MESSAGE-TO-DEVELOPER.txt`,
they carry no information the English ones lack.

Two independent pieces of work:

1. **CraftLink** — a live link between FilmCraft and EffectCraft: right-click clips in FilmCraft →
   the selection opens as a composition in EffectCraft → edits in EffectCraft travel back to the
   FilmCraft timeline about a second later, **without File ▸ Save**.
2. **Layer Styles on 3D text** — Stroke / Inner Shadow / Drop Shadow / and the rest now apply to the
   *rendered* 3D result (along the volume's silhouette), the way After Effects does it. Before this
   change the styles were silently dropped on extruded text and shapes (they never reached a styled
   image at all), and per-character 3D styled the bare glyph planes.

The two parts ship as separate patches and can be reviewed and accepted independently.

| Part | Files | Code lines |
| --- | --- | --- |
| CraftLink, FilmCraft | 25 code (+2 docs) | +1039 / −6 |
| CraftLink, EffectCraft | 10 code (+2 docs) | +839 / −15 |
| 3D Layer Styles, EffectCraft | 8 code (+2 docs) | +272 / −12 |

The two `.md` documents (`CRAFTLINK_ARCHITECTURE.md`, `DYNAMIC_LINK_INTEGRATION.md`, +1365 lines) ride
in both CraftLink patches and are counted separately; `docs/architecture.md` and
`docs/user/guides/add-an-effect.md` (+18 / −3) ride in the 3D patch, also counted separately.
Localisation is three new strings in each of the 16 FilmCraft catalogs
(`crates/ui-egui/src/i18n/*.tsv`). EffectCraft has no `.tsv` catalogs — its menu strings live in
`menus.rs`, which is where the two new entries were added.

**Tests:** FilmCraft `907 passed / 0 failed`; EffectCraft render `405`, gpu `310`, interchange `27`,
aep `671` (3 binaries), engine `1171` — all green except three failures caused *only by the
environment* (see PART IV).

---

## PART I. CraftLink — export from FilmCraft to EffectCraft and the feedback loop

### 1.1 What the user sees

1. In FilmCraft, select clips on the timeline → right-click.
2. **Replace with EffectCraft Composition** replaces the selection with a single linked clip and
   opens the selection as a composition in EffectCraft.
   **Send to EffectCraft…** does the same without touching the FilmCraft timeline (one-shot send).
3. The editor works in EffectCraft (Glow, masks, animation…).
4. About a second after the last edit — or immediately on **File ▸ Save** — EffectCraft re-renders
   the composition **over the same master file**. Saving is optional.
5. FilmCraft notices the change and re-reads the master: the timeline already shows the new image.

The link is a real file on disk, so it survives a restart of either application.

### 1.2 How it is put together

```
FilmCraft: clip selection
   │
   ├─ nest_selection_for_bridge ......... nested sequence shifted to tick 0
   ├─ materialize_generators_for_export . procedural demo generators baked to MP4
   ├─ export FCP7 XML ──────────────────► <name>.xml
   ├─ write_manifest ───────────────────► <name>.craftlink.json   (sidecar)
   └─ bake_linked_master ───────────────► CraftLink/<name>.mov    (ProRes 4444 + alpha + PCM)
   │
   ├─ link_placed_clips_to_master ....... timeline clips point at <name>.mov
   └─ launch_or_notify_effectcraft ...... loopback into a running EffectCraft, else launch
                                          effectcraft.exe <name>.xml

EffectCraft: file.importTimeline
   ├─ import the XML into a composition
   └─ register_import: Item.comment = "craftlink:{...}"   (survives saving the project)

edit in EffectCraft
   │
   ├─ 1 s after the last edit (craftlink_tick) or on File ▸ Save (auto_sync)
   ├─ render_comp ──► <name>.partial.mov ──► rename over <name>.mov
   ├─ spool file: %TEMP%\CraftLink\reload-<pid>-<ms>.notify
   └─ optional loopback: "craftlink.refresh"

FilmCraft: tick_media_refresh sees the notice (polled every 0.5 s while the project shows a link)
   └─ start_media_scan ──► apply_media_stamps ──► re-read <name>.mov
```

Two independent notification channels are deliberate: the spool file needs no socket, token or
port and works even when FilmCraft runs without `--control`; the loopback is only an accelerator.

### 1.3 Data formats

**Interchange timeline — Final Cut Pro 7 XML** (`<xmeml version="4">`). Written by
`filmcraft_interchange::export(..., Format::Fcp7Xml, ...)`, read by
`effectcraft_interchange::import`. Only elements both sides already support are used (`sequence`,
`media/video`, `track`, `clipitem`, `file`, `transitionitem`, `filter`, markers). Nothing
CraftLink-specific is added to the XML — the file stays readable by any FCP7 XML consumer.

**Sidecar manifest** `<name>.craftlink.json` next to the XML:

```json
{
  "craftlink": 1,
  "composition": "CraftLink Comp 01",
  "xml": "C:\\Projects\\Demo\\CraftLink Comp 01.xml",
  "render": "C:\\Projects\\Demo\\CraftLink\\CraftLink Comp 01.mov",
  "autoSync": true,
  "application": "FilmCraft"
}
```

The manifest is optional: an ordinary XML import without it behaves as before and creates no link.

**Storing the link inside EffectCraft** — in the existing comment field of the composition item, as
a single line with the `craftlink:` prefix. This needs **no project schema change** and is saved
with the `.ecproj`. A comment without the prefix is never treated as a link.

### 1.4 FilmCraft changes

| File | Change |
| --- | --- |
| `crates/engine/src/craftlink_bridge.rs` | **new** (718 lines, +718 / -0) — the whole FilmCraft side |
| `crates/engine/src/craftlink_bridge_tests.rs` | **new** — 6 unit tests |
| `crates/engine/src/lib.rs` | module registered (`pub mod craftlink_bridge`, tests module under `#[cfg(test)]`) |
| `crates/engine/src/commands.rs` | 3 commands: `clip.replaceWithCraftLink`, `clip.sendToCraftLink`, `craftlink.refresh` |
| `crates/engine/src/clip_ops.rs` | context-menu ordering |
| `crates/engine/src/media_watch.rs` | `note_media_stamp`, `watches_linked_master` |
| `crates/ui-egui/src/app/frame.rs` | spool-folder polling (0.5 s while a link is shown) |
| `crates/ui-egui/src/panels/timeline/menus.rs` | two context-menu entries |
| `crates/ui-egui/src/i18n/*.tsv` | 3 strings × 16 catalogs |
| `apps/filmcraft/src/main.rs` | flashing console window removed from release builds |

Key functions (`craftlink_bridge.rs`):

| Function | Responsibility |
| --- | --- |
| `next_craftlink_comp_name` | the first free `CraftLink Comp NN` name |
| `link_folder` / `master_path` | `<project folder>/CraftLink`, `%TEMP%/CraftLink` for an unsaved project |
| `nest_selection_for_bridge` | nested sequence from the selection, shifted to tick 0, transitions carried over, boundary transitions trimmed (§1.7) |
| `bake_demo_scene` / `materialize_generators_for_export` | procedural demo generators → real MP4 (EffectCraft has none) |
| `bake_linked_master` | render the sequence to ProRes 4444 + alpha + audio |
| `write_manifest` | sidecar JSON |
| `link_placed_clips_to_master` | media item for the master, clip relinking, baseline media stamp |
| `execute_bridge` | orchestrates both commands |
| `refresh_linked` | `craftlink.refresh` — `media_watch::refresh_changed` + toast |
| `launch_or_notify_effectcraft` | loopback → launch `effectcraft.exe` → otherwise `saved_only` |
| `find_effectcraft_exe` | `EFFECTCRAFT_PATH`, next to the exe, `../EffectCraft/`, `../../EffectCraft/`, `Program Files`, `PATH` |

Commands:

| Command | Menu | Parameters | Result |
| --- | --- | --- | --- |
| `clip.replaceWithCraftLink` | Clip | `{name?, path?, autoSync?, launch?}` | `{sequence, clips, path, name, start, duration, effectcraft, craftlink, replaced:true}` |
| `clip.sendToCraftLink` | Clip | same | same, `replaced:false` |
| `craftlink.refresh` | — | `{}` | the result of `media.refreshChanged` |

`launch:false` does not start EffectCraft (tests, agents); `autoSync:false` disables auto re-render
for that link.

### 1.5 EffectCraft changes

| File | Change |
| --- | --- |
| `crates/engine/src/craftlink.rs` | **new** (525 lines, +525 / -0) — manifest, comment storage, sync, live link, notification |
| `crates/engine/src/cmd/timeline.rs` | `prerender_cx`, `render_comp`, registration on import, 3 command specs (`file.sendToFilmCraft`, `file.craftlinkSync`, `file.craftlinkStatus`) |
| `crates/engine/src/cmd/file.rs` | `auto_sync` after `save` / `saveAs`; `file.import` routes timeline files to `file.importTimeline` |
| `crates/engine/src/cmd.rs` | `pub(crate) mod timeline` |
| `crates/engine/src/lib.rs` | `mod craftlink`, `pub use craftlink::LiveSync`, `Session::craftlink_live` |
| `crates/engine/src/session/dispatch.rs` | `craftlink_note_change` after a command that changed `revision` |
| `crates/engine/src/menus.rs` | File ▸ Export: Send to FilmCraft…, Update CraftLink Render |
| `crates/ui-egui/src/app/frame.rs` | `craftlink_tick(self.now)` in the logic tick |
| `apps/effectcraft/src/main.rs` | double-clicking `.xml` (and `.fcpxml/.otio/.edl/.aaf/.omf`) opens it as a timeline |

Key functions (`craftlink.rs`):

| Function | Responsibility |
| --- | --- |
| `CraftLinkSource::to_value / from_value` | manifest ⇄ struct |
| `CraftLinkSource::to_comment / from_comment` | storage in `Item.comment` |
| `source_for_timeline` / `source_of` | read the sidecar / read the link from a composition's comment |
| `partial_path` | `foo.mov` → `foo.partial.mov` |
| `notify_filmcraft` | spool file + pruning old ones + optional loopback |
| `spawn_render` | render to `.partial.mov` → rename over the master → notify |
| `sync` / `status` | `file.craftlinkSync` / `file.craftlinkStatus` |
| `auto_sync` | re-render every link with `autoSync` after a save |
| `craftlink_note_change` | arm the live link after an edit (skips save/open/import) |
| `craftlink_tick` | render the armed link once the quiet period has elapsed |
| `register_import` | write the comment on import |

**Rendering a whole composition** (`render_comp`): a single `RenderQueueItem` with
`TimeSpan::LengthOfComp`, ProRes, RGBA, `P4444`, audio `Auto`. `prerender_cx` assembles the encoder,
footage, expressions, GPU accelerator and the *Switches Affect Nested Comps* preference — exactly as
a timeline export does, so the background render sees the same project the UI does.

**Save hook** — only `save` and `saveAs`, not `save_to` as a whole: autosave, *Increment and Save*
and recovery saves do not trigger a render. `auto_sync` bails out when a render is already running
(one render at a time).

### 1.6 Live link: edits travel without saving

| Piece | Where | What it does |
| --- | --- | --- |
| `Session::craftlink_live: LiveSync` | `lib.rs`, `craftlink.rs` | debounce state `{dirty, since, seen, comp, rendered}` |
| `craftlink_note_change(id)` | `session/dispatch.rs` | after a command that advanced `revision`: arms the active composition's link; open/revert/import only mark the revision as seen |
| `craftlink_tick(now)` | `ui-egui/src/app/frame.rs` | once no edit happened for `LIVE_DELAY` (1.0 s) — one quiet `spawn_render` |
| `craftlink_live_pending()` | `ui-egui/src/app/frame.rs` | keeps repainting every 120 ms while a render is due |
| `watches_linked_master(project)` | FilmCraft `media_watch.rs` | a project whose master sits inside the `CraftLink` folder polls the spool every 0.5 s |

Rules that had to be respected:

- Only the composition **active** at the time of the edit is re-rendered; switching compositions
  mid-edit does not render a second one.
- One render at a time: an edit during a render is remembered and rendered afterwards, not queued up.
- `autoSync: false` also disables the live link; such a link renders only on `file.craftlinkSync`
  and File ▸ Save.
- The live render is quiet (no toast); only an explicit sync or a save reports back.
- The engine has no clock, so the debounce is driven by the frontend, which passes a monotonic
  `AppClock::now()`.

### 1.7 Bug fixed along the way: the disappearing fade-out

**Symptom.** After sending a selection to EffectCraft, the fade-out at the end of the **last
selected clip** was missing.

**Cause.** A transition can straddle the selection boundary: its outgoing side is selected, its
incoming side is not. The old implementation copied a transition only when **both** sides were
selected, so the boundary transition was dropped and the clip lost its fade.

**Fix.** `nest_selection_for_bridge` now keeps every transition with at least one side in the
selection and turns the missing side into "black":

| Original | In selection | Result |
| --- | --- | --- |
| `from = A`, `to = B` | A only | `from = A`, `to = None` → `end-black` (fade-out) |
| `from = A`, `to = B` | B only | `from = None`, `to = B` → `start-black` (fade-in) |
| `from = None`, `to = B` | B | unchanged |
| `from = A`, `to = None` | A | unchanged |
| both sides outside | — | dropped |

The transition is then **trimmed to the selected clip's bounds**, but only if it actually lost its
other side; a transition with both sides selected (a cross dissolve between two selected clips) is
copied in full. The FCP7 writer emits `start-black` / `end-black`, and the EffectCraft importer
already maps those to Opacity keyframes.

Regression tests: `boundary_transition_becomes_a_one_sided_fade`,
`a_cross_dissolve_between_two_selected_clips_keeps_both_sides`.

### 1.8 Other things in the CraftLink patch

- **Double-clicking an `.xml` in Explorer** opens it in EffectCraft as a timeline (previously only
  `.ecproj` did). `file.import` also recognises `.xml/.fcpxml/.otio/.edl/.aaf/.omf` itself and routes
  them to the timeline importer — so drag-and-drop onto the window works too.
- **Reverse direction**: File ▸ Export ▸ **Send to FilmCraft…** (`file.sendToFilmCraft`) — export XML
  plus launching/notifying FilmCraft, symmetric to the forward direction.
- **Removed the flashing console** on Windows release builds:
  `#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]`
  → `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]` in both `main.rs` files.
  Otherwise launching FilmCraft from EffectCraft briefly flashed a black console window.
- **`note_media_stamp`** records the file's current size/mtime as a baseline. The watcher
  deliberately treats a `None → Some` transition as "unchanged"; without that baseline a save in
  EffectCraft that happened before FilmCraft's first scheduled scan would be missed.

### 1.9 Bug fixed along the way: the first "Send to EffectCraft" failed with `os error 3`

**Symptom.** In a project that had never sent anything to EffectCraft, the **first** bridge run
failed with a bare `os error 3` (ERROR_PATH_NOT_FOUND) and nothing was written. The second attempt
worked, because the folder existed by then.

**Cause.** `execute_bridge` writes the XML through `Services::write_file`, which is atomic: it writes
to a temporary file and renames it over the target. Unlike `write_file_new`, it does **not** create
missing parent folders. The link folder (`<project folder>/CraftLink`) was only created later, inside
`bake_linked_master`, which runs *after* the XML write — so on a fresh project the very first write
had no folder to rename into.

**Fix.** `craftlink_bridge.rs` creates the parent folder before writing the XML:

```rust
if let Some(parent) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
    std::fs::create_dir_all(parent)
        .map_err(|e| EngineError::Other(format!("cannot create {}: {e}", parent.display())))?;
}
```

The error is now a real diagnostic with the path in it instead of a bare OS code.

Regression test: `the_first_bridge_run_creates_its_link_folder` — it points the session at a project
path whose `CraftLink` folder does not exist, runs `clip.sendToCraftLink`, and asserts that the XML
**and** the sidecar manifest were written. It fails without the fix with the original `os error 3`.

---

## PART II. Layer Styles on 3D text (the way After Effects does it)

### 2.1 The problem

Layer Styles (Stroke, Inner Shadow, Drop Shadow, Bevel, Satin, Overlays…) in EffectCraft were
computed in **layer space** from the layer's "flat" content. For a 2D layer that is correct. But 3D
text with **Geometry Options ▸ Extrusion Depth** is not an image any more, it is geometry: side
faces, a bevel, a plane turned under the camera.

In the old build such a layer had no styled image at all. In Advanced 3D, `adv/scene.rs` drew an
extruded text or shape layer with `extruded_layer()` → `extruded_meshes()`, which returns **straight
fill/stroke colours per mesh** and never consults `styles::*`. So the layer styles of an extruded
layer were silently dropped: no stroke, no shadows (see the "Old Version" screenshot in
`screenshots/`). Extrusion is an Advanced 3D feature — Classic 3D's `compose.rs` has no extrude path
— so that is the only renderer where it could show.

Per-character 3D text hit the same wall through a different door: both renderers build it as one
plane per glyph (`adv/scene.rs` `card_layer()` and the Classic `compose.rs` `prepare_all()` both call
`text::per_char_planes()`), and in the old build those planes were plain coloured glyphs with no style
passes at all — the styles of such a layer never reached the screen either.

Flat 3D cards (`card_layer()` → `blend_layer_buf()`) and 2D layers did go through `styles::render()`,
which is why the bug was easy to miss: styles still worked until Geometry Options were switched on.

In After Effects styles are drawn **after** the layer is rendered, that is, against its final image
in the frame. That is exactly what has been reproduced.

### 2.2 The solution: two different paths, by type of 3D text

**(a) Extruded text and shapes** go through the Advanced 3D 2D-compositing path.

```
scene::extrusion_params(layer).is_some() && styles::active(layer)
        │
        ▼
styled_geometry() == true  →  needs_2d_composite() == true
        │
        ▼
draw_run: the layer renders SEPARATELY into its own texture (main scene without it)
        │
        ├─ style_solo():  styles are computed from that render's pixels
        └─ the layer is hidden where the main scene is nearer (depth-buffer occlusion)
        │
        ▼
2D compositing: track matte, Preserve Transparency, blend mode, opacity
```

The key point is **correct opacity handling**: on the 2D path styles see the layer at full opacity,
and opacity then dims the whole finished stack (body + shadows + strokes). To preserve that,
`style_solo` first **divides** the pixels by the layer's opacity, computes the styles, then
**multiplies** the result back. `out_offset()` (region of interest) and `opts.scale`
(Half/Third/Quarter resolution) are taken into account as well, otherwise styles would be displaced
at reduced resolution or in an ROI.

**(b) Per-character 3D text** (the Per-character 3D checkbox) — the 2D path does not fit: each glyph
is a separate plane with its own matrix, and the layer's "overall" silhouette in the frame is not
the sum of the planes. So styles are applied **to each glyph separately**:

```
text::per_char_planes()
    for each glyph:  draw the glyph → styles::styled_buf(...) → push (buf, matrix)
```

`styled_buf()` is a small new wrapper in `styles.rs`: it computes the styles and immediately flattens
the outer passes (Drop Shadow / Outer Glow) into one buffer, because a plane in a 3D scene is a
single texture — separate passes cannot be placed there.

### 2.3 GPU: the same rules, with a read-back

Advanced 3D on the GPU draws into transient buffers and composites with WGSL kernels. For styled
extrusions it works like this (`gpu::walk::draw_split`):

```
resolve_run(special layer)                      normal layer render
finish(...)                                     → GPU image
download(...)                                   → pixels on the CPU  (read-back)
adv::style_solo(r, ctx, layer, img)             → CPU style passes
upload_image(...)                               → back to the GPU
occluded_image(..., iso.depth(), ...)           → hide behind nearer geometry
composite_iso_with(..., 1.0, matte3d)           → 2D matte / blending
```

That required `adv3d::occluded_image()` (hiding an already finished image using the depth buffer of
the isolated render) and the `Resolved::depth()` / `Resolved::size()` accessors.

Styles stay on the CPU on purpose: they are exactly the same passes any 2D layer uses, so the CPU
and GPU images match. The price is one read-back **only for such layers** (extrusion + styles
enabled); ordinary layers pay nothing.

### 2.4 Code changes

| File | Change |
| --- | --- |
| `crates/render/src/three_d/adv.rs` | `styled_geometry()` (pub), `style_solo()` (pub), opacity / ROI / scale handling, `blit()`; `needs_2d_composite()` includes the new layers; `split_run()` no longer discards styled layers wholesale |
| `crates/render/src/styles.rs` | new `styled_buf()` — styles + flattening passes into one buffer |
| `crates/render/src/text.rs` | `per_char_planes()` applies styles to each glyph's plane |
| `crates/render/src/three_d/compose.rs` | comment only (+2 / −0): `prepare_all()` already delegates per-character 3D to `per_char_planes`, so no logic changed here |
| `crates/gpu/src/walk.rs` | `draw_split()`: finish → download → `style_solo` → upload → `occluded_image` |
| `crates/gpu/src/adv3d.rs` | new `occluded_image()`, `Resolved::depth()/size()` accessors |
| `docs/architecture.md` | new paragraph in the **Layer styles** section + edits to the GPU compositing section |
| `docs/user/guides/add-an-effect.md` | one user-facing line: styles follow the rendered 3D |

One detail worth calling out: `blit()` inside `style_solo` puts the finished style buffer back into
the frame at the offset `-flat.offset - out_offset()`. That is the easiest place to be off by one.
It is covered indirectly rather than by a test of its own: `extruded_3d_text_takes_layer_styles`
compares the stroke pixel count of an extrusion against the same text as a flat card (0.7–1.3×),
so styles that landed shifted would break the band; `styles_respect_render_scale` (pre-existing)
pins the half-resolution offset expectation. There is no test that renders an extrusion inside a
region of interest — that is the one branch of the offset maths a reviewer should read by eye.

### 2.5 Tests

**CPU** (`crates/render/src/tests/styles.rs`), all green:

| Test | What it verifies |
| --- | --- |
| `extruded_3d_text_takes_layer_styles` | extruded text + Stroke: more than 500 red stroke pixels on Classic 3D, on an Advanced card and on an Advanced extrusion |
| `extruded_3d_text_takes_inner_and_drop_shadows` | Inner Shadow and Drop Shadow on an extrusion |
| `per_char_3d_text_takes_layer_styles` | per-character 3D + Stroke (Classic and Advanced) |

**GPU** (`crates/gpu/src/tests/adv3d.rs`):

| Test | Result |
| --- | --- |
| `gpu_styled_extrusions_match_cpu` | CPU vs GPU pixel difference **0.000 %** (threshold 2 %), max \|Δ\| = **0.0039** (≈1/255) |

The test also asserts that a styled layer does **not** go through the fast path `prepare_adv_run` and
**does** go through `split_adv_run` — so the routing cannot silently "optimise" itself back.

### 2.6 Known limitations (honest list)

- Styles on extrusions cost one read-back per frame **per such layer**. Ten styled extrusions in a
  frame means ten read-backs. For a typical composition (1–3 text layers) it is imperceptible, but
  it is not free.
- Per-character styles are computed **per glyph**, so a style that conceptually should wrap the
  whole word (for example a Drop Shadow with a large Distance) looks different on overlapping
  letters than in AE. This is a deliberate trade-off: letter planes in 3D do not form a single
  silhouette. For the layer's "overall" silhouette there is the extruded (non per-character) path.
- Inner Bevel still tints the anti-aliased edge over the layer's own colour — that is **old**
  behaviour, unrelated to this work (recorded in `docs/target-app-parity.md`).

---

## PART III. How to verify

### 3.1 Build

```powershell
# FilmCraft — the official build also enables the neural voices feature
cd filmcraft-0.20.1
cargo build --release -p filmcraft --features filmcraft/neural-voices
cargo test -p filmcraft-engine craftlink

# EffectCraft
cd ../effectcraft-0.24.1
cargo build --release -p effectcraft
cargo test -p effectcraft-render --lib
cargo test -p effectcraft-gpu
cargo test -p effectcraft-engine craftlink
```

Two environment details worth knowing:

- **`--features filmcraft/neural-voices`** — the release packaging script
  (`packaging/windows/package.ps1`) builds with this feature, and `default = []` in
  `apps/filmcraft/Cargo.toml`, so a plain `cargo build` produces a binary **without** the neural
  voice-over panel. To match the shipped binary, pass the feature.
- **`CRAFT_FONTS_DIR`** — not needed to build. The release archive already ships the eight
  `assets/fonts/*.ttf` files and the build embeds them. The variable is read only by the
  `effectcraft-ui-egui` integration tests (`crates/ui-egui/tests/it/`), which use a `craft-fonts`
  checkout as their canonical font input; set it if you run those tests, otherwise ignore it.

Toolchain used: `stable-x86_64-pc-windows-gnullvm` (no MSVC `link.exe` on this machine).
The code in the patches is `cargo fmt`-clean, and in EffectCraft `clippy --workspace --all-targets
-- -D warnings` passes. In FilmCraft that gate is red on two **pre-existing** lints — `clippy::needless_bool`
in `crates/hevc/src/filter.rs:506` and `unused_mut` in `crates/engine/src/lib.rs` — that reproduce on
the untouched 0.20.1 archive and are unrelated to these patches. Details and the proof are in §V.4.

### 3.2 CraftLink by hand

1. Open a project in FilmCraft, select 2–3 clips, right-click →
   **Replace with EffectCraft Composition**.
2. EffectCraft should open (or accept via the control channel) with the composition.
3. Add, say, a Glow. **Do not save.**
4. After ~1–2 seconds switch back to FilmCraft: the timeline should already show the Glow.
5. Check the fade-out at the end of the last clip (regression from §1.7).
6. Close and reopen both applications: the link must survive.

### 3.3 3D styles by hand

1. A composition using the **Advanced 3D** renderer (Composition Settings ▸ Advanced).
2. A text layer, 3D switch on, **Geometry Options ▸ Extrusion Depth > 0**.
3. **Layer ▸ Layer Styles ▸ Stroke** — the stroke must follow the projected silhouette of the volume.
4. Add **Inner Shadow** and **Drop Shadow** — the shadows must fall from the volume, not from flat
   text.
5. Enable **Per-character 3D** — stroke and shadows must hug each letter.
6. Rotate the camera or the layer: styles must stick to the geometry of the frame.
7. Compare with GPU acceleration off: the images must match.
8. If you do not want to build before reading: `screenshots/old-version.png` is step 3 failing on the
   unpatched release, `screenshots/new-version.png` is it working with the patch.

---

## PART IV. Test results

| Crate | Result |
| --- | --- |
| `filmcraft-engine` | **907 passed**, 0 failed, 3 ignored |
| `effectcraft-render` | **405 passed**, 0 failed, 1 ignored |
| `effectcraft-gpu` | **310 passed**, 0 failed, 2 ignored |
| `effectcraft-interchange` | 27 passed |
| `effectcraft-aep` | 60 + 608 + 3 passed (three test binaries) |
| `effectcraft-engine` | **1171 passed**, 6 ignored, **1 failed** — see below |
| `effectcraft-ui-egui` | **540 passed**, 4 ignored, **2 failed** — see below |

**Both failures are environment-only and unrelated to the changes:**

1. `effectcraft-engine::tests::blender::blender_export_defaults_and_script_escaping` — the test runs
   the generated script through `python3` as a syntax oracle. It fails at
   `crates/engine/src/tests/blender.rs:181` ("generated script must parse even with quoted/Unicode
   paths"). The cause is the machine, not the code: `python3` resolves to
   `%LOCALAPPDATA%\Microsoft\WindowsApps\python3.exe`, the 0-byte Microsoft Store stub, which exits
   with code 9009 and parses nothing. The test only skips when `python3` is *absent* (a
   `NotFound` error); a stub that exists but runs nothing counts as a failure. With a real
   Python 3 on `PATH` as `python3`, the test passes.
2. `effectcraft-ui-egui::native_menu::tests::every_engine_entry_is_in_the_native_menu` and
   `panels::templates::tests::my_custom_rgb_dialog` — the system locale on this machine is Russian
   while the tests expect English labels (`File` vs `Файл`, `My Custom RGB...` vs `Свой RGB...`).
   This is the intended behaviour of the *Match System* mode; on an English-locale machine the tests
   pass. It reproduces on a clean checkout without these changes.

**Builds:** `effectcraft.exe` — 114 559 488 B (SHA-256 `D8D88C58…`), `filmcraft.exe` — 75 913 728 B.
These are the sizes the builds came out at on this machine; the executables themselves are **not**
part of this package and are not needed — the recipient builds their own from the patched source.

---

## PART V. What is in the package

```
craftlink-3dstyles-report/
├── REPORT.md                                    ← this file (English)
├── MESSAGE-TO-DEVELOPER.txt                     ← ready-to-copy message (English)
├── ОТЧЁТ.md                                     ← the same report in Russian
├── СООБЩЕНИЕ-РАЗРАБОТЧИКУ.txt                   ← the same message in Russian
├── 01-filmcraft-0.20.1-craftlink.patch            27 files, CraftLink, FilmCraft side
├── 02-effectcraft-0.24.1-craftlink.patch          12 files, CraftLink, EffectCraft side
├── 03-effectcraft-0.24.1-3d-layer-styles.patch    10 files, Layer Styles on 3D
├── craftlink-patcher.exe                        applies all three patches by itself (no git, no Python)
├── CRAFTLINK_ARCHITECTURE.md                     the full CraftLink description (EN + RU), ≈68 KB
├── DYNAMIC_LINK_INTEGRATION.md                   historical document (marked DEPRECATED)
├── FILES.txt                                     file list + sizes + sha256 (the 45 in changed-files/)
├── screenshots/
│   ├── old-version.png                           EffectCraft 0.24.1: extruded text, Stroke on, no stroke drawn
│   └── new-version.png                           the same layer with this patch: the stroke follows the glyphs
├── changed-files/
│   ├── filmcraft-0.20.1/…                        complete files (just replace them)
│   └── effectcraft-0.24.1/…                      same
├── patcher/                                      Rust source of craftlink-patcher.exe (self-contained)
│   ├── Cargo.toml
│   └── src/{main.rs, patch.rs, sha256.rs}
├── installer/                                    Rust source of CraftLink-Installer.exe (no-Rust route)
│   ├── Cargo.toml
│   ├── payload/EMBEDDED.txt                      offset + length + sha256 of the two carried builds
│   └── src/{main.rs, sha256.rs}
```

The two finished programs the installer carries (181.5 MB together) are **not** included here: they
are build output, reproducible from the patches with one command, and keeping copies would turn a
5 MB review package into a 187 MB one. `installer/payload/EMBEDDED.txt` records, for each of them, the
offset, length and SHA-256 inside the shipped `CraftLink-Installer.exe`, so the shipped binary can be
checked against this package without the copies (§PART VI, Option 0b).

`changed-files/` holds **45** files to copy over the release trees (43 `.rs`/`.tsv` code files and the
2 documents under `docs/`). 42 of them replace an existing file; the other 3 are new files
(`crates/engine/src/craftlink_bridge.rs`, `craftlink_bridge_tests.rs` in FilmCraft and
`crates/engine/src/craftlink.rs` in EffectCraft), so add those rather than overwrite. Two more files —
`CRAFTLINK_ARCHITECTURE.md` and `DYNAMIC_LINK_INTEGRATION.md` — are new files that live in the package
root instead, because both CraftLink patches add them identically; copy them into the repository root
by hand. So 47 distinct files are new or changed, and the per-patch file counts in the summary table
add up to more than that because those two documents are in both CraftLink patches.

`craftlink-patcher.exe` is an alternative to all of that: it applies the three patches itself, from
data compiled into it (§PART VI, Option 0). Its source is in `patcher/`, so it can be audited and
rebuilt. It is not needed by any of the other routes and nothing else in the package depends on it.

**For people who just want to install it.** A second, separate folder ships next to this one:

```
craftlink-quick-patch/
├── CraftLink-Installer.exe   the finished builds, carried inside; it explains itself when run
├── Uninstall.bat             the same executable in removal mode, for a one-click undo
├── README.txt                how to run it, what it changes, how to remove it (RU + EN)
└── screenshots/              the same before/after pair
```

This is a different executable from `craftlink-patcher.exe`, and deliberately so. The patcher edits
**sources** and needs `cargo` to produce a program; the installer carries the **finished programs**
and needs nothing at all. A person who only wants the features points it at their portable folder
and is done — no Rust, no git, no source tree, no build step.

It shares no code with the patcher and modifies nothing in this package. Its source is in
`installer/`, and its payload is byte-identical to what the patches produce, so the developer package
stays the authority: anything the installer writes can be reproduced from the patches.

### V.1 If you are an AI reviewer reading this

Everything below is arranged so a review can be done by checking a claim against the file that
backs it. Nothing here needs to be taken on trust.

| Claim | Where to check |
| --- | --- |
| The link is stored without a schema change | `CraftLinkSource::to_comment` / `from_comment` in `crates/engine/src/craftlink.rs` |
| Saving is optional | `Session::craftlink_tick` in `craftlink.rs` + its call in `crates/ui-egui/src/app/frame.rs` |
| A save still renders immediately | `auto_sync` calls in `crates/engine/src/cmd/file.rs` (`save`, `saveAs` only) |
| Opening or reverting does not render | `LIVE_IGNORED` in `craftlink.rs` |
| Styled extrusions take the 2D path | `styled_geometry` + `needs_2d_composite` in `crates/render/src/three_d/adv.rs`; asserted in `gpu_styled_extrusions_match_cpu` (`crates/gpu/src/tests/adv3d.rs`) |
| Styles see the layer at full opacity | the divide/re-apply around `style_solo` in `adv.rs` |
| Per-glyph styling exists | `styled_buf` in `crates/render/src/styles.rs`, called from `per_char_planes` in `text.rs` |
| GPU and CPU agree | `gpu_styled_extrusions_match_cpu` in `crates/gpu/src/tests/adv3d.rs` |
| The fade-out bug is fixed | `nest_selection_for_bridge` in `craftlink_bridge.rs` + the two regression tests |
| The first bridge run creates its folder | `create_dir_all` before `write_file` in `execute_bridge` + `the_first_bridge_run_creates_its_link_folder` |
| Menu labels match the customer's wording | `crates/engine/src/commands.rs` (`Send to EffectCraft…`, `Replace with EffectCraft Composition`) while internal ids stay `clip.*CraftLink`; the context-menu entries and their 16 catalogs are in `crates/ui-egui/src/panels/timeline/menus.rs` and `crates/ui-egui/src/i18n/*.tsv` |

Two things a reviewer might otherwise flag as defects, both deliberate:

1. The two `.md` documents are inside the CraftLink patches rather than a separate commit. They
   describe exactly these changes, so splitting them would leave the code undocumented in the patch
   that introduces it. Say the word and they can be peeled into their own patch.
2. The styles for styled extrusions run on the CPU even in the GPU path. That is intentional (§2.3):
   it is the same code an ordinary 2D layer runs, which is why the CPU/GPU parity test can pass at
   0.000 %.

### V.2 How the patches were verified

The patches were generated by diffing the working tree against the **pristine release archives**
(the source zips of FilmCraft 0.20.1 and EffectCraft 0.24.1 — the same folders you already have).
Each patch was then applied to a freshly extracted clean archive and the result was compared
**byte-for-byte** with the working tree (with line endings normalised) — a full match.

No **code** file appears in two patches, and no file is ever patched twice inside one repository.
The two documents (`CRAFTLINK_ARCHITECTURE.md`, `DYNAMIC_LINK_INTEGRATION.md`) intentionally appear
in both CraftLink patches, because each side of the link gets its own copy in its own repository
root: patch 01 adds them to the FilmCraft tree, patch 02 to the EffectCraft tree.

The same gate was run after the `os error 3` fix (§1.9) was added, and separately for the
regression test: with the `create_dir_all` guard removed, the test fails with exactly
`os error 3`; with the guard back, it passes.

Line endings are normalised to LF (the release archives are LF; some files on disk were CRLF), so
the patches contain no meaningless multi-thousand-line diffs caused by `\r\n`.

The patches are plain unified diffs — no `diff --git` or `index` lines, `/dev/null` for created
files — so `git apply`, `patch -p1` and `applypatch` all read them. `patch` and `git` are not
installed on this machine, so the byte-for-byte gate above was executed by a small independent
applier that consumes exactly the line counts each `@@` header declares; every hunk in all three
patches checks out.

### V.3 How best to send it (recommendation)

**The main rule: keep the two independent changes apart.** CraftLink and the 3D styles share no
code and touch different files. Fold everything into one patch and the reviewer reads ~1200 code lines at
once and cannot accept one part while deferring the other. That is why the package contains three
patches, not one.

**Structure of the message (the text, not the files).** The most common mistake is sending an
archive with no legend, leaving the reviewer half an hour of figuring out what is what. Suggested
shape:

1. **One line of essence** — what changed for the user, no technical detail.
2. **Two or three lines per part** — what exactly and why.
3. **What is attached** — the package file list, one explanatory line each.
4. **How to apply** — exact commands.
5. **How to verify** — concrete steps in the application.
6. **What fails and why it is not the patch** — up front, so nobody wastes time on it
   (blender/python3 and the locale). This matters more than it sounds: otherwise the first thing the
   reader sees is a red test and they conclude the patch is broken.
7. **Known limitations** — honestly, so it does not read as "everything is perfect".

A ready-to-copy text sits next to this file: **`MESSAGE-TO-DEVELOPER.txt`** (Russian version:
`СООБЩЕНИЕ-РАЗРАБОТЧИКУ.txt`).

**Attachment format.** In decreasing order of convenience for the recipient:

| Approach | When it fits | Drawbacks |
| --- | --- | --- |
| **Git branch / PR** (if repository access exists) | ideal: per-file review, comments, CI | needs access; our repositories are local, without `.git` |
| **One `.zip` with patches + report + `craftlink-patcher.exe`** (what is here) | the recipient patches a folder with one command and can undo it completely | needs a Windows PC; the executable is unsigned |
| **One `.zip` with patches + report** | the recipient runs `git apply` and sees a real diff | needs `git` or `patch` |
| **One combined `.patch`** | if the person likes `patch -p1` and a single file | ~1200 code lines in one file, hard to review in parts |
| **Complete files** (`changed-files/`) | if the recipient has **exactly** 0.20.1 / 0.24.1 | any version skew overwrites their own edits |

The practical choice is the **zip with patches, `changed-files/` and `craftlink-patcher.exe`
inside**: the recipient picks whether to apply a diff, run one executable, or simply replace files.

**What to attach beyond code.** For CraftLink nothing special — it is verified by following the
steps. For the 3D styles the package already carries a before/after pair in `screenshots/`, because
"the stroke follows the silhouette" cannot be verified in words:

- `screenshots/old-version.png` — EffectCraft 0.24.1 without the patch: 3D text with Geometry Options
  ▸ Extrusion Depth, Layer Styles ▸ Stroke enabled (Size 3.0, Position Outside, red), and **no stroke
  drawn at all**. The only red in that frame is the viewer's selection box around the layer.
- `screenshots/new-version.png` — the same kind of layer with this patch applied: the red stroke is
  drawn around the projected silhouette of every glyph.

Two things the pair deliberately does **not** prove, so state them in words or add a frame if the
reviewer asks: the camera is not rotated between shots (it is a different angle, so the two frames are
illustrative rather than pixel-comparable), and neither frame uses Per-character 3D. A short screen
recording of the camera orbit and one more frame with Per-character 3D ticked close that gap — both
are a switch on an ordinary text layer.

Screenshots belong in the same zip (`screenshots/`), not in a separate message, so they cannot get
lost.

**File naming.** Patches are named `<number>-<product>-<topic>.patch`. The number sets the apply
order, the product says which repository, the topic says what to review. If you rename them, keep
those three parts — they save the recipient more time than any prose.

**What to avoid:**

- Do not send only screenshots without code, or vice versa.
- Do not send a patch without stating the base version — "applies to 0.20.1 / 0.24.1" must be in the
  first line of the message.
- Do not mix `CRAFTLINK_ARCHITECTURE.md` (documentation) with code in one patch if the reviewer asks
  to split them — here it is deliberate, since the document describes exactly these changes.

### V.4 What was run before packaging (so you don't have to take it on faith)

Every claim in this report was re-checked against the source right before the package was
assembled. These are the exact commands and their results:

| Gate | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all --check` | clean in both repositories (pristine 0.20.1 / 0.24.1 are also clean, so this is a real gate, not a waivered one) |
| Lints, EffectCraft | `cargo clippy --workspace --all-targets -- -D warnings` | **passes**, exit 0 |
| Lints, FilmCraft | `cargo clippy --workspace --all-targets -- -D warnings` | **fails, and it fails on the pristine 0.20.1 archive too** — see the note below |
| Lints, files these patches touch | `cargo clippy -p filmcraft-engine -p effectcraft-engine -p effectcraft-render -p effectcraft-gpu -p effectcraft-ui-egui --all-targets` | no diagnostic points at any file the patches add or change |
| CraftLink tests | `cargo test -p filmcraft-engine craftlink` | 6 passed |
| CraftLink tests | `cargo test -p effectcraft-engine craftlink` | 10 passed |
| 3D style tests | `cargo test -p effectcraft-render --lib` | 405 passed, 1 ignored |
| GPU parity | `cargo test -p effectcraft-gpu` | 310 passed, 2 ignored |
| Patch integrity | each patch applied to a fresh extract of the release zip | byte-for-byte equal to the working tree |
| Patcher's output | `craftlink-patcher.exe apply` on a fresh extract of both archives, then a byte comparison with the working tree | 27/27 and 22/22 files identical |
| Patcher refuses other releases | `apply` on a folder that only looks like a release | exit 1, message names the line, **nothing written** |
| Patcher refuses a portable build | `apply` on a real `*-windows-x64-portable` folder | exit 1, explains that it holds the compiled exe and gives the patch → build → copy route, **nothing written** |
| Patcher revert | `revert` on the patched tree | 23/23 replaced files restored to the pristine bytes |
| Patcher uninstall | `uninstall` on a patched tree, then a whole-tree comparison with the release | 3487 files before, 3487 after; **0 missing, 0 extra, 0 changed** |
| Patcher is not stale | the three patches and `FILES.txt` searched for inside the executable | all four present byte-for-byte, so the binary was built from these exact files |

**About the FilmCraft lint gate — please read this one, it is the only red item.**

`AGENTS.md` requires `cargo xtask ci`, which is `cargo fmt --check` followed by
`cargo clippy --workspace --all-targets -- -D warnings`. On this machine that clippy run stops on
**one** lint in `crates/hevc/src/filter.rs:506`:

```
error: this `if` guard returns a bool literal and is followed by another
   --> crates/hevc/src/filter.rs:506:9
```

This is a **pre-existing** lint, not ours. Proof: extracting the untouched 0.20.1 source archive and
running the
same command on it produces the identical error. The line is unchanged by the
patches — `filmcraft-hevc` is not among the 27 files they touch.

Why it surfaces here at all: the lint (`clippy::needless_bool`, newer refinement) fires under the
locally installed Rust 1.100.0, while the project declares `rust-version = "1.95"`. On the compiler
the project targets, the workspace gate is green.

A second pre-existing lint is also present, and under `-D warnings` it is an error as well:
`unused_mut` on `let mut dir = std::fs::DirBuilder::new();` in the temp-folder helper —
`crates/engine/src/lib.rs:129` in the patched tree (`:126` in the pristine archive, because the
patch registers a module above it). It is triggered only when `filmcraft-hevc` is not compiled
along the way, and it too reproduces on the untouched archive.

Deliberate decision: these two were **left untouched**. Changing `filmcraft-hevc` would mix an
unrelated fix into a patch that is supposed to be about CraftLink. If you want a green
`cargo xtask ci` on this toolchain, it is a two-line change; say the word and it can ship as a
separate fourth patch.

The three `effectcraft` test failures described in PART IV are likewise environment-only —
`python3` and the system locale — and reproduce on a clean checkout without these patches.

### V.5 The patches follow the repository house rules

Worth stating explicitly, because `AGENTS.md` is where a reviewer will look first:

- **Never crash** — no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`
  or `unsafe` in any non-test code added by these patches. Verified by scanning every touched file
  outside its `#[cfg(test)]` module.
- **Narrow, tested fixes** — the fade-out bug (§1.7) has two regression tests that fail without the
  fix, and the missing-folder bug (§1.9) has one; the 3D style work has three CPU tests and one GPU
  parity test.
- **Everything is a command** — the FilmCraft entries are registered in `commands.rs` with ids,
  labels and params; the internal ids keep the agreed names (`clip.replaceWithCraftLink`,
  `clip.sendToCraftLink`, `craftlink.refresh`) while the visible labels changed.
- **No schema change** — the link rides in the existing `Item.comment` field, so old projects and
  old builds keep loading.
- **No new dependencies** — the patches add none, so the licence allowlist is untouched.

---

## PART VI. How to apply the patches

### Option 0 — `craftlink-patcher.exe` (nothing to install)

This is the easiest route and the one meant for a quick local check: the three patches and
`FILES.txt` are compiled into the executable, so it needs no `git`, no `patch`, no Python, no
network, and no files beside it.

```
craftlink-patcher.exe                     interactive: shows what would change, then asks
craftlink-patcher.exe check <folder>      dry run; writes nothing
craftlink-patcher.exe apply <folder>      run the check, then patch (asks before writing)
craftlink-patcher.exe uninstall <folder>  put the folder back exactly as it was
craftlink-patcher.exe revert <folder>     restore only the overwritten files
craftlink-patcher.exe list                show the embedded patches
craftlink-patcher.exe help                usage
```

`<folder>` is the root of an unpacked release — the folder that contains
`apps/filmcraft/Cargo.toml` or `apps/effectcraft/Cargo.toml`. The release is detected from that
file; the executable also works when the folder is dragged into the console window.

**Not an installed or portable build.** A portable folder holds `filmcraft.exe` / `effectcraft.exe`,
which are already compiled, while these patches edit source files (`.rs`, `.tsv`, `.md`) and the
application must be rebuilt afterwards. Pointing the patcher at such a folder is refused with an
explanation and a three-step route instead of a bare error: patch the sources, run
`cargo build --release`, then copy `target/release/<product>.exe` over the one in the portable
folder (keeping the old file as a backup). The same applies to an installation made by the launcher.

Safety, deliberately stricter than `patch -p1`:

- **`check` never writes.** It applies every hunk in memory only.
- **`apply` plans first and writes second.** If one context line anywhere does not match, it
  stops and touches nothing — it cannot half-patch a folder that is not the release it was built
  for. The message names the file, the line, what was expected and what was found.
- **Every file it overwrites is copied to `_craftlink-backup/<date>/`** inside the release folder
  before being written, and the files it *creates* are listed in that folder too, so the undo is
  complete rather than partial.
- **After writing it re-hashes each file** against the embedded `FILES.txt` and reports
  `Verified against FILES.txt: 25 file(s) ok` (FilmCraft) or `20` (EffectCraft), plus the two root
  documents the manifest does not list. A mismatch is reported as an error, not a warning.
- **`uninstall` is a real undo:** it restores the replaced files, deletes the added ones and prunes
  the folders that become empty, returning the tree to the release state. Verified as a whole-tree
  comparison — 3487 files before, 3487 after, with nothing missing, extra or changed. A file the
  user edited after patching is deliberately kept and named.
- `revert` is the narrower variant: it restores only the overwritten files and leaves the added ones
  in place, saying so.

The expected numbers are `27` files written for FilmCraft (23 replaced + 4 new) and `22` for
EffectCraft (19 replaced + 3 new); the two `.md` documents are among the new ones on both sides.
Deleting `_craftlink-backup/` does no harm once the build and tests are green.

If the patches are ever regenerated, the executable must be rebuilt — it carries a copy of them.
Rebuilding is `cd patcher && cargo build --release`; it reads the patches from the package root,
needs only a Rust toolchain and no network. `cargo test` in the same folder runs twelve tests: two
of them fail if a patch and the embedded `FILES.txt` disagree, one fails if a patch no longer
parses, and one holds the wording of the refusal for a portable build.

### Option 0b — `CraftLink-Installer.exe` (the no-Rust route, for end users)

`craftlink-patcher.exe` edits sources and therefore still needs `cargo` to turn them into a program.
For someone who has no Rust toolchain — which is almost everyone outside the project — that is the
whole difficulty. `CraftLink-Installer.exe` removes it: the two finished builds are compiled into the
executable with `include_bytes!`, so it installs without compiling anything.

It lives in `craftlink-quick-patch/` (its source is here, in `installer/`).

```
CraftLink-Installer.exe                     interactive: finds, shows, confirms
CraftLink-Installer.exe check <folder>      dry run; reports what would be replaced
CraftLink-Installer.exe install <folder>    replace the program (a backup is made first)
CraftLink-Installer.exe uninstall <folder>  put the previous program back
CraftLink-Installer.exe list                show what is carried inside
CraftLink-Installer.exe help                usage
```

`<folder>` is a **portable** folder — the one holding `filmcraft.exe` or `effectcraft.exe` next to
`portable.txt`. The installer looks at that folder, one level below it (archives usually unpack as
`name/name/`), and two levels below it, so a person may name either level. Naming a folder that
contains the unpacked folders of both products installs both in one run. Dragging a folder onto the
executable does the same as naming it.

What it does, and what it refuses:

- **Identifies by content, not by name.** The program already in the folder must hash to the official
  build (SHA-256 pinned per product); anything else — a different version, or a build already changed
  — is refused instead of overwritten.
- **Plans all of it before writing any of it.** If any one folder does not fit, the run stops and
  nothing changes anywhere, including the folders that did fit.
- **Replaces exactly one file** in each portable folder. `data/`, `portable.txt`, the licences and
  `*-cli.exe` are not touched.
- **Backs up first** to `_craftlink-backup/<date>/` inside the portable folder, with a `REPLACED.txt`
  naming what that run replaced; `uninstall` restores from it and was verified to return the official
  bytes exactly.
- **`check` writes nothing** at all.

The cost is size: it carries a 72.3 MB and a 109.2 MB build, so the executable is ~182 MB (~67 MB
zipped). That is the price of not requiring a compiler.

Those two builds are **not** in this package — they are build output, reproducible from the patches,
and copying them here would turn a 5 MB review package into a 187 MB one. Instead
`installer/payload/EMBEDDED.txt` records the offset, length and SHA-256 of each inside the shipped
installer, which is enough to verify a received `CraftLink-Installer.exe` against this package:

```powershell
# both carried programs, cut out of the shipped exe, must match EMBEDDED.txt
python - <<'PY'
import hashlib, io
exe = open("CraftLink-Installer.exe", "rb").read()
for row in io.open("craftlink-3dstyles-report/installer/payload/EMBEDDED.txt", encoding="utf-8"):
    if row.startswith("#") or row.startswith("name\t") or not row.strip():
        continue
    name, offset, length, digest = row.split("\t")
    blob = exe[int(offset):int(offset) + int(length)]
    print(name, hashlib.sha256(blob).hexdigest() == digest)
PY
```

Rebuilding after the payload changes: `cd installer && cargo build --release`. It needs
`payload/filmcraft.exe` and `payload/effectcraft.exe` in place — copy them from
`target/release/` of the patched trees — and no network. `cargo test` in the same folder runs fifteen
tests: two SHA-256 vectors, a check that each payload is a real PE file rather than a placeholder, a
check that each pinned release hash differs from the build it carries, refusal of a foreign build,
and the folder-discovery rules (direct, nested, both products, empty).

### Option A — git

The package is a folder of files; unpack the two source archives next to it, so the layout is:

```
somewhere/
├── 01-filmcraft-0.20.1-craftlink.patch      ← from this package
├── 02-effectcraft-0.24.1-craftlink.patch    ← from this package
├── 03-effectcraft-0.24.1-3d-layer-styles.patch
├── filmcraft-0.20.1/                        ← unzipped source archive
└── effectcraft-0.24.1/                      ← unzipped source archive
```

Then:

```powershell
# The two release folders and the patch files sit side by side.
cd filmcraft-0.20.1
git apply --check ../01-filmcraft-0.20.1-craftlink.patch   # dry run
git apply         ../01-filmcraft-0.20.1-craftlink.patch

cd ../effectcraft-0.24.1
git apply --check ../02-effectcraft-0.24.1-craftlink.patch
git apply         ../02-effectcraft-0.24.1-craftlink.patch
git apply --check ../03-effectcraft-0.24.1-3d-layer-styles.patch
git apply         ../03-effectcraft-0.24.1-3d-layer-styles.patch
```

Paths inside the patches are `a/… b/…`, so `-p1` (the `git apply` default) is correct.
The two EffectCraft patches touch disjoint files, so their order does not matter.

### Option B — without git (the `patch` utility)

```powershell
# The two release folders and the patch files sit side by side.
cd filmcraft-0.20.1
patch -p1 < ../01-filmcraft-0.20.1-craftlink.patch

cd ../effectcraft-0.24.1
patch -p1 < ../02-effectcraft-0.24.1-craftlink.patch
patch -p1 < ../03-effectcraft-0.24.1-3d-layer-styles.patch
```

### Option C — just replace the files

Copy the contents of `changed-files/filmcraft-0.20.1/` and `changed-files/effectcraft-0.24.1/` over
the corresponding repositories. **Only if the versions match exactly** (0.20.1 and 0.24.1). Cross-check
with `FILES.txt` (size + sha256). Then copy `CRAFTLINK_ARCHITECTURE.md` and
`DYNAMIC_LINK_INTEGRATION.md` from the package root into the **root of the FilmCraft and EffectCraft
repositories** — they are new files, not replacements, which is why they are not in `changed-files/`.

---

## PART VII. Timeline of the work

| Step | What was done |
| --- | --- |
| 1 | Read the code on both sides: `interchange`, `cmd/timeline`, `media_watch`, plus the earlier bridge prototype described in `DYNAMIC_LINK_INTEGRATION.md` |
| 2 | Wrote `craftlink_bridge.rs` (FilmCraft): nesting the selection, baking generators, ProRes 4444 master, sidecar, clip relinking, launch/loopback |
| 3 | Wrote `craftlink.rs` (EffectCraft): manifest, comment storage, `render_comp`, `auto_sync`, spool notification |
| 4 | Added commands on both sides, menu entries, 16 localisation catalogs |
| 5 | Found and fixed the missing fade-out at the selection boundary + two regression tests |
| 6 | Added the live link (1 s debounce driven by the frontend tick) + 0.5 s spool polling in FilmCraft |
| 7 | Renamed the menu entries per the customer's requirement (internal ids untouched) |
| 8 | Analysed Layer Styles rendering: how the 2D path computes styles and how Advanced 3D builds the scene |
| 9 | Implemented `styled_geometry` / `style_solo` (CPU) + routing into `needs_2d_composite` |
| 10 | Implemented per-character styles via `styled_buf` in `per_char_planes` |
| 11 | Implemented the GPU path: read-back, styles on the CPU, `occluded_image` |
| 12 | Wrote 3 CPU tests and 1 GPU test comparing against the CPU |
| 13 | Found and fixed the `os error 3` on the first bridge run (`create_dir_all` before writing the XML) + a regression test |
| 14 | Ran the full test suites of both products, built the releases, updated the portable folders |
| 15 | Updated the documentation (`docs/architecture.md`, the user guide) and assembled this package |

---

## PART VIII. Known limitations

**CraftLink**

- **Not per-keystroke.** The master re-renders a second after the last edit, on File ▸ Save, or via
  `file.craftlinkSync` — not on every intermediate frame of a slider drag. The next step would be a
  frame cache.
- **No streaming.** Latency = the time to render the whole composition plus one UI frame.
- **One render at a time.** An edit during a render is remembered and rendered afterwards.
- **The live link exists in the desktop build only.** In the browser build the link lives in a folder
  on disk and there is no background thread, so the render would be inline.
- **`autoSync: false`** disables the live link too; such a link only refreshes manually.

**3D styles**

- One read-back per styled extruded layer per frame (see §2.6).
- Per-character styles are computed per glyph, not for the word's overall silhouette.
- Inner Bevel tints the anti-aliased edge — old behaviour, not part of this work.

**Environment**

- The Blender test needs a working `python3` on `PATH`; here `python3` resolves to the 0-byte
  Microsoft Store stub, which exits with code 9009 (see PART IV).
- Two UI tests expect an English locale (this machine is Russian).

---

## Code pointers

- CraftLink, full description (EN + RU): `CRAFTLINK_ARCHITECTURE.md` in this package.
- Layer Styles on 3D: `docs/architecture.md`, the **Layer styles** section (second paragraph).
- User documentation: `docs/user/guides/add-an-effect.md`, the **Layer styles** section.