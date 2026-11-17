# CraftLink — live composition link between FilmCraft and EffectCraft

**Status:** implemented, compiled, unit-tested (17 Nov 2026)
**Scope:** FilmCraft 0.20.1 + EffectCraft 0.24.1 (Rust)
**Audience:** a developer or an AI agent who has to understand, maintain, port or re-implement the feature

> This document describes the implementation as shipped in the three patches next to it
> (`01-…filmcraft`, `02-…effectcraft`, `03-…3d-layer-styles`). Two bugs found while testing are
> fixed in those patches and covered by regression tests: the disappearing fade-out at a selection
> boundary, and the first bridge run failing with `os error 3` because the link folder did not exist
> yet. Both are described in `REPORT.md` §1.7 and §1.9.

---

## 0. Clean-room statement

CraftLink was written from the plain behaviour of the two applications and from **open, publicly
documented formats only**:

| Purpose | Format | Specification |
| --- | --- | --- |
| Timeline exchange | Final Cut Pro 7 XML (`<xmeml version="4">`) | open, widely documented interchange format |
| Linked master | Apple ProRes 4444 in QuickTime, RGBA + PCM | openly documented codec and container |
| Sidecar metadata | JSON (RFC 8259) | public standard |
| Live notice | a file in the temp folder + JSON Lines over loopback | public standard |

No proprietary project format, private protocol, shared library, decompiled binary, vendor
documentation, string table or user interface of any third-party product was consulted. The
feature is **not** an implementation of any commercial "dynamic link" product and is not
affiliated with, endorsed by, or derived from one. Names, module layout, identifiers and the
wire format below are original to this project. Anyone can rebuild CraftLink from this document
alone.

---

## 1. What CraftLink does

1. The editor selects one or more episodes (clips) on a FilmCraft timeline and right-clicks.
2. **Replace with EffectCraft Composition** replaces the selection with a linked master clip and
   opens the selection as a composition in EffectCraft.
   **Send to EffectCraft** does the same without changing the FilmCraft timeline.
3. The editor works in EffectCraft (adds Glow, masks, animation…).
4. About a second after the last edit — or immediately on **File ▸ Save** — EffectCraft renders the
   composition back over the same master file. Saving is optional: the link follows the edits.
5. FilmCraft notices the change and re-reads the master, so the timeline shows the edit.

The linked master is a real file on disk, so the link survives restarts, and either application
can be closed and reopened without losing the connection.

---

## 2. Architecture

```
FilmCraft timeline
  selection
     │
     ▼
nest_selection_for_bridge ──► nested sequence (clips shifted to tick 0)
     │
     ├─ materialize_generators_for_export ──► procedural demos baked to MP4
     ├─ export FCP7 XML  ───────────────────► <name>.xml
     ├─ write_manifest   ───────────────────► <name>.craftlink.json
     └─ bake_linked_master ─────────────────► CraftLink/<name>.mov  (ProRes 4444 + alpha + PCM)
     │
     ▼
link_placed_clips_to_master ──► timeline clips now point at <name>.mov
     │
     ▼
launch_or_notify_effectcraft ──► effectcraft.exe <name>.xml
                                  (or loopback file.importTimeline when it is running)
     │
     ▼
EffectCraft: file.importTimeline
     ├─ import the XML into a composition
     └─ register_import: Item.comment = "craftlink:{...}"   (survives project reload)
     │
     ▼
editor changes the composition
     │
     ▼
1 s after the last edit (craftlink_tick) or on File ▸ Save (auto_sync)
     ├─ render_comp ──► <name>.partial.mov ──► rename over <name>.mov
     ├─ spool: %TEMP%\CraftLink\reload-<pid>-<ms>.notify
     └─ optional loopback: engine.execute "craftlink.refresh"
     │
     ▼
FilmCraft tick_media_refresh sees the notice (polled every 0.5 s while the project shows a link)
     └─ start_media_scan ──► apply_media_stamps ──► re-read <name>.mov
     │
     ▼
timeline shows the EffectCraft edit
```

Two independent notification paths exist on purpose: the spool file needs no socket, no token
and no port, and works even when FilmCraft was started without `--control`; the loopback request
is a best-effort speed-up.

---

## 3. Data formats

### 3.1 Exchange timeline — Final Cut Pro 7 XML

Written by FilmCraft with
`filmcraft_interchange::export(&project, sequence, Format::Fcp7Xml, &options)`.
Read by EffectCraft through `effectcraft_interchange::import`, which delegates parsing to
`filmcraft_interchange::import_with`.

Only elements both applications already support are used: `sequence`, `media/video`,
`media/audio`, `track`, `clipitem`, `file`, `transitionitem`, `filter` (Basic Motion, Opacity,
Audio Levels, Audio Pan), markers. Nothing CraftLink-specific is added to the XML, so the file
remains readable by any FCP7-XML consumer.

### 3.2 Sidecar manifest — `<name>.craftlink.json`

Written next to the XML by FilmCraft (`write_manifest`), read by EffectCraft
(`source_for_timeline`).

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

| Field | Type | Meaning |
| --- | --- | --- |
| `craftlink` | integer | manifest version, currently `1` |
| `composition` | string | the composition's name in FilmCraft |
| `xml` | string | absolute path of the exchanged timeline |
| `render` | string | absolute path of the linked master |
| `autoSync` | bool | keep the master in sync automatically: re-render a second after an edit and on every save (default `true`) |
| `application` | string | who created the link (`"FilmCraft"`) |

The manifest is optional: a plain XML import without it behaves exactly like a normal timeline
import and creates no link.

### 3.3 Link persistence inside EffectCraft — `Item.comment`

EffectCraft has no field for external links, so the manifest is stored in the composition item's
existing comment field, on one line, prefixed with `craftlink:`:

```
craftlink:{"craftlink":1,"composition":"CraftLink Comp 01","xml":"…","render":"…","autoSync":true,"application":"FilmCraft"}
```

This needs **no project schema change** and is saved and loaded with the project. A comment that
does not start with `craftlink:` is never interpreted as a link.

Code: `CraftLinkSource::to_comment` / `CraftLinkSource::from_comment`.

### 3.4 Live notice — `%TEMP%\CraftLink\reload-<pid>-<ms>.notify`

- **Writer:** EffectCraft, after a successful render (`notify_filmcraft`).
- **Body:** `{"craftlink":1,"render":"C:\\…\\<name>.mov"}`.
- **Reader:** FilmCraft, every frame while the window is open
  (`crates/ui-egui/src/app/frame.rs`, `craftlink_notify_pending` / `craftlink_clear_notices`).
- **Lifetime:** consumed once a media scan starts; files older than 10 minutes are pruned by the
  writer, so a crashed or closed receiver leaves no permanent litter.

The receiver does not parse the path; it only needs to know that something changed, because the
regular media watcher re-stamps and re-reads every file the project uses.

### 3.5 Optional loopback request

If `FILMCRAFT_CONTROL_TOKEN` is present in the environment (64 hex characters, inherited by the
EffectCraft process that FilmCraft launched), EffectCraft also sends one JSON line to FilmCraft's
control server:

```json
{"token":"<64 hex>","method":"engine.execute","params":{"command":"craftlink.refresh","params":{}}}
```

Ports tried: `filmcraft_control.json` (working directory and temp folder, `port` field) then
`9876`. Without a token FilmCraft keeps its listener closed by design, so this path is optional.

---

## 4. FilmCraft changes

### 4.1 Files

| File | Change |
| --- | --- |
| `crates/engine/src/craftlink_bridge.rs` | **new** — the whole FilmCraft side |
| `crates/engine/src/craftlink_bridge_tests.rs` | **new** — 6 unit tests |
| `crates/engine/src/lib.rs` | module registered (`pub mod craftlink_bridge`, tests module under `#[cfg(test)]`) |
| `crates/engine/src/commands.rs` | 3 commands registered |
| `crates/engine/src/clip_ops.rs` | context-menu ordering |
| `crates/engine/src/media_watch.rs` | `Session::note_media_stamp`, `watches_linked_master` |
| `crates/ui-egui/src/app/frame.rs` | spool polling in `tick_media_refresh` (0.5 s while a link is shown), after the existing `has_media_files` check |
| `crates/ui-egui/src/panels/timeline/menus.rs` | two context-menu labels |
| `crates/ui-egui/src/i18n/*.tsv` | 3 strings × 16 catalogs |
| `apps/filmcraft/src/main.rs` | console window removed from release builds (`windows_subsystem`, no longer gated on `debug_assertions`) |

### 4.2 Function reference (`crates/engine/src/craftlink_bridge.rs`)

| Function | Responsibility |
| --- | --- |
| `next_craftlink_comp_name(s)` | first unused `CraftLink Comp NN` name |
| `link_folder(s)` | `<project folder>/CraftLink`, or `%TEMP%/CraftLink` for an unsaved project |
| `master_path(s, name)` | `<link_folder>/<name>.mov` |
| `manifest_path(xml)` | `<stem>.craftlink.json` next to the XML |
| `nest_selection_for_bridge(s, name, replace)` | builds the nested sequence from the selection, shifts it to tick 0, copies transitions, trims boundary transitions (see §6), optionally replaces the selection on the active timeline |
| `bake_demo_scene(scene, duration)` | renders one procedural demo generator to a real H.264 MP4 |
| `materialize_generators_for_export(s, nid)` | replaces generator media items in the nested sequence with those MP4 files |
| `bake_linked_master(s, nid, path, duration)` | renders the sequence to ProRes 4444 with alpha and audio |
| `write_manifest(s, xml, master, name, auto_sync)` | writes the sidecar JSON |
| `link_placed_clips_to_master(s, placed, name, master)` | adds a media item for the master, repoints the placed clips at it, and baselines its media stamp |
| `execute_bridge(s, p, replace)` | orchestration for both send commands |
| `refresh_linked(s, p)` | `craftlink.refresh` — `media_watch::refresh_changed` + toast |
| `launch_or_notify_effectcraft(xml)` | loopback first, then launch `effectcraft.exe`, else report `saved_only` |
| `find_effectcraft_exe()` | `EFFECTCRAFT_PATH`, next to the exe, `../EffectCraft/`, `../../EffectCraft/`, `Program Files`, `PATH` |

### 4.3 Commands

| Command | Menu | Parameters | Result |
| --- | --- | --- | --- |
| `clip.replaceWithCraftLink` | Clip | `{name?, path?, autoSync?, launch?}` | `{sequence, clips, path, name, start, duration, effectcraft, craftlink, replaced:true}` |
| `clip.sendToCraftLink` | Clip | `{name?, path?, autoSync?, launch?}` | same, `replaced:false` |
| `craftlink.refresh` | — | `{}` | `media.refreshChanged` result |

`launch:false` skips starting EffectCraft (tests, agents); `autoSync:false` makes EffectCraft
stop re-rendering on save for this link.

The `craftlink` object in the result is:

```json
{"linked": true, "render": "…\\CraftLink\\CraftLink Comp 01.mov", "autoSync": true}
```

`linked` is `false` when the master could not be read back (the timeline then keeps the nested
composition, which renders the same content), and `error` explains why.

### 4.4 Media watcher addition

`Session::note_media_stamp(path)` records the current size/mtime of a media file as the baseline.
It is called right after the linked master is inserted, because the watcher deliberately treats
`None → Some` as "not changed" (a file that did not exist at the last look is not a file that
changed). Without this baseline, a save in EffectCraft that happened before FilmCraft's first
timed scan would be missed.

### 4.5 UI

- Timeline clip context menu: **Replace with EffectCraft Composition**, **Send to EffectCraft**.
- Clip menu: same commands.
- `tick_media_refresh` forces a scan as soon as a `.notify` file exists, even when
  *Automatically refresh growing files* is off.
- A project that shows a linked master (`media_watch::watches_linked_master`: a media file inside
  a `CraftLink` folder) polls the spool folder every 0.5 s, so a live render shows at once instead
  of on the next timed scan (which can be a minute away). The periodic scan still obeys
  *Automatically refresh growing files*; the CraftLink notice does not.

---

## 5. EffectCraft changes

### 5.1 Files

| File | Change |
| --- | --- |
| `crates/engine/src/craftlink.rs` | **new** — manifest, comment persistence, sync, live link, notice |
| `crates/engine/src/cmd/timeline.rs` | `prerender_cx`, `render_comp`, import registration, 3 command specs (`file.sendToFilmCraft`, `file.craftlinkSync`, `file.craftlinkStatus`) |
| `crates/engine/src/cmd.rs` | `pub(crate) mod timeline` |
| `crates/engine/src/cmd/file.rs` | `auto_sync` after `save` / `saveAs` |
| `crates/engine/src/lib.rs` | `mod craftlink`, `pub use craftlink::LiveSync`, `Session::craftlink_live` |
| `crates/engine/src/session/dispatch.rs` | `craftlink_note_change` after a command that changed `revision` |
| `crates/ui-egui/src/app/frame.rs` | `craftlink_tick(self.now)` in the logic tick |
| `crates/engine/src/menus.rs` | File ▸ Export entries |
| `crates/engine/src/tests/timeline_interchange.rs` | test `send_to_filmcraft_exports_xml` |
| `apps/effectcraft/src/main.rs` | a timeline file (`.xml`/`.fcpxml`/`.otio`/`.edl`/`.aaf`/`.omf`) given on the command line opens as a timeline; console window removed |

### 5.2 Function reference (`crates/engine/src/craftlink.rs`)

| Function | Responsibility |
| --- | --- |
| `CraftLinkSource::from_value / to_value` | manifest ⇄ struct |
| `CraftLinkSource::to_comment / from_comment` | persistence in `Item.comment` |
| `manifest_path(xml)` | sidecar path |
| `source_for_timeline(xml)` | read the sidecar |
| `source_of(s, cid)` | read the link from the composition's comment |
| `partial_path(render)` | `foo.mov` → `foo.partial.mov` |
| `notify_filmcraft(render)` | spool file + pruning + optional loopback |
| `notify_filmcraft_control()` | authenticated JSON-lines request |
| `spawn_render(s, cid, src, wait, quiet)` | render to `.partial.mov`, rename over the master, notify |
| `sync(s, p)` | `file.craftlinkSync` |
| `status(s, p)` | `file.craftlinkStatus` |
| `auto_sync(s)` | re-render every `autoSync` link after a save |
| `craftlink_note_change(id)` | arm the live link after an edit (skips saves, opens, imports) |
| `craftlink_tick(now)` | render the armed link once the quiet period is over |
| `craftlink_live_pending()` | whether a live render is owed (the frontend's repaint hint) |
| `register_import(project, comps, xml)` | write the comment on import |

### 5.3 Whole-composition render (`crates/engine/src/cmd/timeline.rs`)

`render_comp(cx, project, cid, path, progress)` builds one `RenderQueueItem`:

```
time_span      = TimeSpan::LengthOfComp
output.format  = OutputFormat::ProRes
channels       = Channels::Rgba
prores_profile = ProResProfile::P4444
audio          = AudioOutput::Auto
output.output  = path
```

and hands it to the session's `Exporter`. `prerender_cx(s)` collects the encoder, footage source,
expression host, GPU accelerator and the *Switches Affect Nested Comps* preference, exactly as
timeline export does, so a render in the background sees the same project as the UI.

### 5.4 Commands

| Command | Menu | Parameters | Result |
| --- | --- | --- | --- |
| `file.craftlinkSync` | File ▸ Export ▸ Update CraftLink Render | `{comp?, background?}` | `{comp, render, xml}` (or `{job}` when `background:true`) |
| `file.craftlinkStatus` | — | `{comp?}` | `{linked, composition, xml, render, autoSync, application, exists, bytes, modified}` |
| `file.sendToFilmCraft` | File ▸ Export ▸ Send to FilmCraft… | `{comp?, path?}` | `{comp, name, path, export, filmcraft}` — `filmcraft.status` is one of `connected_to_control_server`, `launched`, `launch_failed`, `saved_only` |

`file.sendToFilmCraft` is the reverse direction: it exports the active composition as a Final Cut Pro
XML, then hands the file to a running FilmCraft over the control channel
(`engine.execute` → `file.import`), or starts `filmcraft.exe` on it, or leaves the `.xml` on disk
(`saved_only`). `find_filmcraft_exe` looks at `FILMCRAFT_PATH`, next to the exe, `../FilmCraft/`,
`../../FilmCraft/`, `Program Files` and `PATH`, in that order.

### 5.5 Save hook

`crates/engine/src/cmd/file.rs`:

```rust
fn save(s, p)    { let r = save_to(s, &path)?; crate::craftlink::auto_sync(s); Ok(r) }
fn save_as(s, p) { let r = save_to(s, path)?;  crate::craftlink::auto_sync(s); Ok(r) }
```

`save_to` itself is **not** hooked, so autosave, *Increment and Save* and recovery saves do not
trigger renders. `auto_sync` skips when a `craftlink` task is already running (one render at a
time) and only considers links whose `autoSync` is true.

### 5.6 Live link (no save needed)

The master follows the edits without File ▸ Save:

| Piece | Where | What it does |
| --- | --- | --- |
| `Session::craftlink_live: LiveSync` | `lib.rs`, `craftlink.rs` | `{dirty, since, seen, comp, rendered}`: the debounce state |
| `craftlink_note_change(id)` | `session/dispatch.rs` | after any command that advanced `revision`: arms the link for the composition that was active; saves, opens, reverts and imports only mark the revision as seen |
| `craftlink_tick(now)` | `ui-egui/src/app/frame.rs` | once no edit arrived for `LIVE_DELAY` (1.0 s), starts one quiet `spawn_render` |
| `craftlink_live_pending()` | `ui-egui/src/app/frame.rs` | keeps a 120 ms repaint coming while a render is owed |
| `watches_linked_master(project)` | FilmCraft `media_watch.rs` | a project with a media file inside a `CraftLink` folder polls the spool every 0.5 s |

Rules:

- Only the composition that was **active** when the edit happened is re-rendered; switching comps
  mid-edit does not render the other one.
- One render at a time: an edit during a render is remembered and rendered after it, not stacked.
- `autoSync: false` disables the live link as well; only `file.craftlinkSync` and *File ▸ Save*
  render such a link.
- The live render is quiet (no toast); only the explicit sync and saves report.
- The engine has no clock, so the debounce is driven by the frontend's monotonic `AppClock::now()`
  passed into `craftlink_tick`.

---

## 6. The missing fade-out fix

### Symptom

After sending a selection to EffectCraft, a fade-out at the end of the **last selected clip** was
missing in EffectCraft (reported with a screenshot).

### Cause

A transition can straddle the selection boundary: its outgoing side is selected, its incoming
side is not. The previous bridge copied a transition only when **both** sides were selected, so
the boundary transition was dropped and the outgoing clip lost its fade.

### Fix

`nest_selection_for_bridge` now keeps every transition that has at least one side in the
selection and turns the missing side into "black":

| Original | Selection | Result |
| --- | --- | --- |
| `from = A`, `to = B` | A only | `from = A`, `to = None` → `end-black` (fade-out) |
| `from = A`, `to = B` | B only | `from = None`, `to = B` → `start-black` (fade-in) |
| `from = None`, `to = B` | B | unchanged → `start-black` |
| `from = A`, `to = None` | A | unchanged → `end-black` |
| both sides outside | — | dropped |
| both sides become `None` | — | dropped |

The transition is then **trimmed to the selected clip's own extent** — but only when it actually
lost its other side to black; a transition with both sides in the selection (a cross dissolve
between two selected clips) is copied whole:

- fade-out (`from` selected): `end = min(transition.end, clip.end)`,
  `duration = end - start`;
- fade-in (`to` selected): `start = max(transition.start, clip.start)`,
  `duration = end - start`.

The FCP7 writer emits `start-black` / `end-black` from `(from, to)` (`write_transition`,
`crates/interchange/src/fcp7.rs`). EffectCraft's importer already maps those to opacity
keyframes (`crates/interchange/src/import.rs`):

```rust
if tr.to == Some(clip)          { fades.push((start, end, 0.0, 1.0)); } // fade in
if tr.from == Some(clip) && tr.to.is_none() { fades.push((start, end, 1.0, 0.0)); } // fade out
```

Because the transition was trimmed, the fade-out now covers the clip's last frames instead of
extending past its end.

Regression test: `boundary_transition_becomes_a_one_sided_fade` and
`a_cross_dissolve_between_two_selected_clips_keeps_both_sides`
(`crates/engine/src/craftlink_bridge_tests.rs`).

---

## 7. Live-sync sequence, step by step

1. **FilmCraft** — the editor selects clips, runs `clip.replaceWithCraftLink`.
2. `nest_selection_for_bridge` creates the nested sequence (and, for *replace*, swaps the
   selection for a nested-sequence clip).
3. `materialize_generators_for_export` bakes procedural demo generators to MP4 (EffectCraft has no
   FilmCraft generators).
4. `filmcraft_interchange::export` writes `<name>.xml`.
5. `bake_linked_master` renders `CraftLink/<name>.mov` (ProRes 4444 + alpha + PCM) and
   `write_manifest` writes `<name>.craftlink.json`.
6. `link_placed_clips_to_master` adds a media item for the master, repoints the placed clips at
   it and records the media stamp.
7. `launch_or_notify_effectcraft` asks a running EffectCraft to import the XML (loopback) or
   starts `effectcraft.exe <xml>`.
8. **EffectCraft** — `file.importTimeline` imports the XML, then `register_import` writes
   `craftlink:{…}` into the composition's comment.
9. The editor adds effects (Glow, masks…) and saves.
10. `auto_sync` → `spawn_render` → `render_comp` writes `<name>.partial.mov`, renames it over
    `<name>.mov`, then `notify_filmcraft` writes the spool notice.
11. **FilmCraft** — `tick_media_refresh` sees the notice, starts a media scan, the scan re-reads
    the changed master, the timeline repaints with the new render.

Latency: the render time (whole composition) plus one UI frame. There is no per-frame streaming.

---

## 8. Settings and defaults

| Setting | Where | Default |
| --- | --- | --- |
| *Automatically refresh growing files* | FilmCraft ▸ Settings ▸ Media | on |
| *Refresh growing files every* | FilmCraft ▸ Settings ▸ Media | 60 s |
| `autoSync` | command parameter / manifest | `true` |
| `launch` | command parameter | `true` |

The CraftLink notice forces a scan even when the automatic refresh is off.

---

## 9. Build and test

The project builds with the `stable-x86_64-pc-windows-gnullvm` toolchain (no MSVC `link.exe`).

```powershell
# FilmCraft
cd filmcraft-0.20.1
cargo build --release -p filmcraft
cargo test -p filmcraft-engine craftlink

# EffectCraft
cd effectcraft-0.24.1
cargo build --release -p effectcraft
cargo test -p effectcraft-engine craftlink
cargo test -p effectcraft-engine menus::tests
```

Expected CraftLink tests:

```
craftlink_bridge_tests::craftlink_fails_when_no_clips_selected            ok
craftlink_bridge_tests::replace_with_craftlink_creates_xml_and_replaces_on_timeline   ok
craftlink_bridge_tests::send_to_craftlink_keeps_original_timeline         ok
craftlink_bridge_tests::boundary_transition_becomes_a_one_sided_fade      ok
craftlink_bridge_tests::a_cross_dissolve_between_two_selected_clips_keeps_both_sides ok
craftlink_bridge_tests::the_first_bridge_run_creates_its_link_folder         ok
craftlink::tests::a_manifest_round_trips_through_a_comment                ok
craftlink::tests::a_plain_comment_is_not_a_link                           ok
craftlink::tests::the_manifest_sits_next_to_the_timeline                  ok
craftlink::tests::a_partial_render_keeps_the_extension                    ok
craftlink::tests::an_edit_arms_the_live_link                              ok
craftlink::tests::opening_a_document_does_not_arm_the_live_link           ok
craftlink::tests::the_live_link_waits_for_the_quiet_period                ok
craftlink::tests::a_new_edit_restarts_the_quiet_period                    ok
craftlink::tests::an_unlinked_composition_is_not_rendered_by_the_live_link ok
craftlink::tests::a_link_with_auto_sync_off_waits_for_a_save              ok
```

After a release build, copy `target/release/filmcraft.exe` and
`target/release/effectcraft.exe` into their portable folders (keep the previous binary as
`.exe.bak`).

---

## 10. Porting CraftLink to another pair of applications

The feature needs only four capabilities, none of them proprietary:

1. **Write an interchange timeline.** Any format both sides read (FCP7 XML here).
2. **Write and read a sidecar JSON** next to it.
3. **Render a composition to a movie file** both sides can decode (ProRes 4444 here).
4. **Notice that the movie changed** (file watch, spool file, or socket).

Minimum implementation:

```
send(selection):
    seq = nest(selection)
    xml = export(seq, FCP7)
    write(xml)
    write(xml.stem + ".craftlink.json", {craftlink:1, composition, xml, render, autoSync})
    render(seq, render)
    replace_on_timeline(selection, render)   # optional
    open_or_notify(other_app, xml)

import(xml):
    comp = import_timeline(xml)
    link = read(xml.stem + ".craftlink.json")
    if link: remember(comp, link)

save(project):
    for comp in project where remembered(comp).autoSync:
        render(comp, remembered(comp).render)
        notify(remembered(comp).render)

edit(project):                     # after any command that changed the document
    link = remembered(active_comp)
    if link and link.autoSync:     # saves, opens and imports are not edits
        armed = link
        armed.due = now + 1.0      # a new edit restarts the quiet period

tick(now):                         # the frontend's frame loop, monotonic clock
    if armed and now >= armed.due and no render running:
        render(armed.comp, armed.render)
        notify(armed.render)
        armed = none

notify(render):
    write(temp/"CraftLink"/"reload-<pid>-<ms>.notify", {craftlink:1, render})
```

`remember` can be a comment field (as here), a project-level table, or a database — the format
does not matter as long as it is saved with the project.

---

## 11. Known limits

- **Not per-keystroke.** The master is re-rendered a second after the last edit, on *File ▸ Save*
  (or on `file.craftlinkSync`), not for every intermediate frame of a slider drag. A per-frame
  cache would be the next step.
- **Whole-composition render.** EffectCraft renders the complete composition, not only the
  changed region.
- **Absolute paths.** Moving the project folder breaks the link until the manifest is edited.
- **One render at a time.** An edit or save during a running CraftLink render is remembered; the
  next tick or save picks it up.
- **The composition that was active.** Only the linked composition that was active when the edit
  happened is re-rendered: editing another composition that uses it as a nested precomp does not
  update the master.
- **Master size.** ProRes 4444 with alpha is large; keep the exchange folder on a fast disk.
- **ProRes availability.** Both builds must include the ProRes encoder/decoder (they do).

---

## 12. Internationalisation

Three new user-visible strings in FilmCraft — the English source plus the 16 translation
catalogs under `crates/ui-egui/src/i18n/`:

| Source (English) | Where |
| --- | --- |
| `Replace with EffectCraft Composition` | Clip menu, timeline context menu |
| `Send to EffectCraft…` | Clip menu, timeline context menu |
| `Update CraftLink Renders` | command palette / agent registry |

Missing translations fall back to English, and the i18n report test always passes. The strings
are translated in `ru.tsv`, `uk.tsv`, `de.tsv`, `es.tsv`, `fr.tsv`, `it.tsv`, `pt-br.tsv`,
`pl.tsv`, `cs.tsv`, `sv.tsv`, `tr.tsv`, `ja.tsv`, `ko.tsv`, `zh-cn.tsv`, `id.tsv`, `bs.tsv`.

---

## 13. Changelog

| Date | Change |
| --- | --- |
| 14 Nov 2026 | CraftLink: renamed from the earlier `EffectCraft` bridge; live master, manifest, spool notice, save auto-sync, `craftlink.refresh`, `file.craftlinkSync`, `file.craftlinkStatus` |
| 14 Nov 2026 | Fixed the missing fade-out at the end of a selected clip (boundary transitions become one-sided fades) |
| 16 Nov 2026 | Trimming applies only to a transition that lost its other side; a cross dissolve between two selected clips is copied whole |
| 16 Nov 2026 | Menu items renamed to **Replace with EffectCraft Composition** / **Send to EffectCraft…**; the live link re-renders 1 s after the last edit, without File ▸ Save (`LiveSync`, `craftlink_tick`); FilmCraft polls the spool every 0.5 s while the project shows a link |
| 17 Nov 2026 | The link folder is created before the first write, so the first bridge run no longer fails with `os error 3`; regression test `the_first_bridge_run_creates_its_link_folder` |
| 14 Nov 2026 | `Session::note_media_stamp` so a render written before the first timed scan is still noticed |

---
---

# CraftLink — живая связь композиций FilmCraft и EffectCraft

**Статус:** реализовано, собрано, покрыто юнит-тестами (17 ноября 2026)
**Версии:** FilmCraft 0.20.1 + EffectCraft 0.24.1 (Rust)
**Для кого:** разработчик или ИИ-агент, которому нужно понять, поддерживать, портировать или
заново реализовать функцию

> Этот документ описывает реализацию в том виде, в каком она поставляется тремя патчами рядом
> (`01-…filmcraft`, `02-…effectcraft`, `03-…3d-layer-styles`). Два бага, найденных при
> тестировании, исправлены в этих патчах и покрыты регресс-тестами: пропадающий fade-out на
> границе выделения и падение первого запуска моста с `os error 3` из-за отсутствующей папки
> ссылки. Оба описаны в `REPORT.md` §1.7 и §1.9.

---

## 0. Заявление о чистой комнате

CraftLink написан исходя из обычного поведения двух приложений и **только по открытым,
публично задокументированным форматам**:

| Назначение | Формат | Спецификация |
| --- | --- | --- |
| Обмен таймлайном | Final Cut Pro 7 XML (`<xmeml version="4">`) | открытый, широко описанный формат обмена |
| Связанный мастер | Apple ProRes 4444 в QuickTime, RGBA + PCM | открыто описанные кодек и контейнер |
| Боковые метаданные | JSON (RFC 8259) | публичный стандарт |
| Уведомление | файл во временной папке + JSON Lines по loopback | публичный стандарт |

Не использовались проприетарный формат проекта, закрытый протокол, разделяемая библиотека,
дизассемблированный бинарник, документация производителя, таблицы строк или интерфейс
стороннего продукта. Функция **не является** реализацией какого-либо коммерческого продукта
«динамической связи» и не связана с ним. Имена, структура модулей, идентификаторы и формат
сообщений ниже — оригинальные для этого проекта. CraftLink можно полностью воспроизвести по
этому документу.

---

## 1. Что делает CraftLink

1. Редактор выделяет один или несколько эпизодов (клипов) на таймлайне FilmCraft и вызывает
   контекстное меню.
2. **Replace with EffectCraft Composition** заменяет выделение связанным мастер-клипом и
   открывает выделение как композицию в EffectCraft.
   **Send to EffectCraft** делает то же самое, не меняя таймлайн FilmCraft.
3. Редактор работает в EffectCraft (добавляет Glow, маски, анимацию…).
4. Примерно через секунду после последней правки — или сразу по **File ▸ Save** — EffectCraft
   рендерит композицию поверх того же мастер-файла. Сохранять проект не обязательно: связь
   следует за правками.
5. FilmCraft замечает изменение и перечитывает мастер — таймлайн показывает правку.

Связанный мастер — реальный файл на диске, поэтому связь переживает перезапуск, а любое из
приложений можно закрыть и открыть без потери связи.

---

## 2. Архитектура

```
Таймлайн FilmCraft
  выделение
     │
     ▼
nest_selection_for_bridge ──► вложенная последовательность (клипы сдвинуты к тику 0)
     │
     ├─ materialize_generators_for_export ──► процедурные демо запекаются в MP4
     ├─ экспорт FCP7 XML  ──────────────────► <имя>.xml
     ├─ write_manifest    ──────────────────► <имя>.craftlink.json
     └─ bake_linked_master ─────────────────► CraftLink/<имя>.mov  (ProRes 4444 + альфа + PCM)
     │
     ▼
link_placed_clips_to_master ──► клипы таймлайна теперь указывают на <имя>.mov
     │
     ▼
launch_or_notify_effectcraft ──► effectcraft.exe <имя>.xml
                                  (или loopback file.importTimeline, если он уже запущен)
     │
     ▼
EffectCraft: file.importTimeline
     ├─ импорт XML в композицию
     └─ register_import: Item.comment = "craftlink:{...}"   (переживает перезагрузку проекта)
     │
     ▼
редактор меняет композицию
     │
     ▼
через 1 с после последней правки (craftlink_tick) или по File ▸ Save (auto_sync)
     ├─ render_comp ──► <имя>.partial.mov ──► переименование поверх <имя>.mov
     ├─ spool: %TEMP%\CraftLink\reload-<pid>-<ms>.notify
     └─ необязательный loopback: engine.execute "craftlink.refresh"
     │
     ▼
FilmCraft tick_media_refresh видит уведомление (опрос каждые 0.5 с, пока в проекте есть связь)
     └─ start_media_scan ──► apply_media_stamps ──► перечитывание <имя>.mov
     │
     ▼
таймлайн показывает правку из EffectCraft
```

Два независимых пути уведомления сделаны намеренно: spool-файлу не нужны сокет, токен и порт,
он работает даже если FilmCraft запущен без `--control`; loopback-запрос — лишь необязательное
ускорение.

---

## 3. Форматы данных

### 3.1 Таймлайн обмена — Final Cut Pro 7 XML

Пишется FilmCraft через
`filmcraft_interchange::export(&project, sequence, Format::Fcp7Xml, &options)`.
Читается EffectCraft через `effectcraft_interchange::import`, который разбирает файл через
`filmcraft_interchange::import_with`.

Используются только элементы, которые оба приложения уже поддерживают: `sequence`,
`media/video`, `media/audio`, `track`, `clipitem`, `file`, `transitionitem`, `filter`
(Basic Motion, Opacity, Audio Levels, Audio Pan), маркеры. В XML не добавляется ничего
специфичного для CraftLink, поэтому файл остаётся читаемым любым потребителем FCP7 XML.

### 3.2 Боковой манифест — `<имя>.craftlink.json`

Пишется рядом с XML в FilmCraft (`write_manifest`), читается в EffectCraft
(`source_for_timeline`).

```json
{
  "craftlink": 1,
  "composition": "CraftLink Comp 01",
  "xml": "C:\\Проекты\\Демо\\CraftLink Comp 01.xml",
  "render": "C:\\Проекты\\Демо\\CraftLink\\CraftLink Comp 01.mov",
  "autoSync": true,
  "application": "FilmCraft"
}
```

| Поле | Тип | Смысл |
| --- | --- | --- |
| `craftlink` | целое | версия манифеста, сейчас `1` |
| `composition` | строка | имя композиции в FilmCraft |
| `xml` | строка | абсолютный путь к файлу обмена |
| `render` | строка | абсолютный путь к связанному мастеру |
| `autoSync` | bool | держать мастер в синхронизации автоматически: рендер через секунду после правки и при каждом сохранении (по умолчанию `true`) |
| `application` | строка | кто создал связь (`"FilmCraft"`) |

Манифест необязателен: обычный импорт XML без него ведёт себя как всегда и связи не создаёт.

### 3.3 Хранение связи внутри EffectCraft — `Item.comment`

В EffectCraft нет поля для внешних связей, поэтому манифест хранится в существующем поле
комментария элемента композиции, одной строкой, с префиксом `craftlink:`:

```
craftlink:{"craftlink":1,"composition":"CraftLink Comp 01","xml":"…","render":"…","autoSync":true,"application":"FilmCraft"}
```

Это **не требует** изменения схемы проекта и сохраняется вместе с ним. Комментарий, не
начинающийся с `craftlink:`, связью никогда не считается.

Код: `CraftLinkSource::to_comment` / `CraftLinkSource::from_comment`.

### 3.4 Уведомление — `%TEMP%\CraftLink\reload-<pid>-<ms>.notify`

- **Пишет:** EffectCraft после успешного рендера (`notify_filmcraft`).
- **Содержимое:** `{"craftlink":1,"render":"C:\\…\\<имя>.mov"}`.
- **Читает:** FilmCraft каждый кадр, пока открыто окно
  (`crates/ui-egui/src/app/frame.rs`, `craftlink_notify_pending` / `craftlink_clear_notices`).
- **Время жизни:** файл поглощается, как только начинается сканирование; файлы старше
  10 минут удаляет писатель, поэтому упавший получатель не оставляет мусора.

Получатель не разбирает путь: ему достаточно знать, что что-то изменилось, — обычный
наблюдатель медиа заново снимет штампы и перечитает все файлы проекта.

### 3.5 Необязательный loopback-запрос

Если в окружении есть `FILMCRAFT_CONTROL_TOKEN` (64 hex-символа; переменная наследуется
процессом EffectCraft, который запустил FilmCraft), EffectCraft дополнительно отправляет одну
JSON-строку в control-сервер FilmCraft:

```json
{"token":"<64 hex>","method":"engine.execute","params":{"command":"craftlink.refresh","params":{}}}
```

Порты: `filmcraft_control.json` (рабочая папка и временная папка, поле `port`), затем `9876`.
Без токена FilmCraft держит слушатель закрытым, поэтому этот путь необязателен.

---

## 4. Изменения в FilmCraft

### 4.1 Файлы

| Файл | Изменение |
| --- | --- |
| `crates/engine/src/craftlink_bridge.rs` | **новый** — вся сторона FilmCraft |
| `crates/engine/src/craftlink_bridge_tests.rs` | **новый** — 6 юнит-тестов |
| `crates/engine/src/lib.rs` | регистрация модуля (`pub mod craftlink_bridge`, тестовый модуль под `#[cfg(test)]`) |
| `crates/engine/src/commands.rs` | 3 команды |
| `crates/engine/src/clip_ops.rs` | порядок в контекстном меню |
| `crates/engine/src/media_watch.rs` | `Session::note_media_stamp`, `watches_linked_master` |
| `crates/ui-egui/src/app/frame.rs` | опрос spool-файлов в `tick_media_refresh` (каждые 0.5 с, пока показана связь), после существующей проверки `has_media_files` |
| `crates/ui-egui/src/panels/timeline/menus.rs` | две подписи контекстного меню |
| `crates/ui-egui/src/i18n/*.tsv` | 3 строки × 16 каталогов |
| `apps/filmcraft/src/main.rs` | убрано консольное окно у релизных сборок (`windows_subsystem`, больше не под `debug_assertions`) |

### 4.2 Функции (`crates/engine/src/craftlink_bridge.rs`)

| Функция | Ответственность |
| --- | --- |
| `next_craftlink_comp_name(s)` | первое свободное имя `CraftLink Comp NN` |
| `link_folder(s)` | `<папка проекта>/CraftLink` или `%TEMP%/CraftLink` для несохранённого проекта |
| `master_path(s, name)` | `<link_folder>/<имя>.mov` |
| `manifest_path(xml)` | `<stem>.craftlink.json` рядом с XML |
| `nest_selection_for_bridge(s, name, replace)` | строит вложенную последовательность из выделения, сдвигает к тику 0, копирует переходы, обрезает граничные переходы (см. §6), при необходимости заменяет выделение на активном таймлайне |
| `bake_demo_scene(scene, duration)` | рендерит один процедурный демо-генератор в настоящий H.264 MP4 |
| `materialize_generators_for_export(s, nid)` | заменяет генераторы во вложенной последовательности этими MP4 |
| `bake_linked_master(s, nid, path, duration)` | рендерит последовательность в ProRes 4444 с альфой и звуком |
| `write_manifest(s, xml, master, name, auto_sync)` | пишет боковой JSON |
| `link_placed_clips_to_master(s, placed, name, master)` | добавляет медиа-элемент мастера, перенаправляет на него клипы и фиксирует штамп |
| `execute_bridge(s, p, replace)` | оркестрация обеих команд отправки |
| `refresh_linked(s, p)` | `craftlink.refresh` — `media_watch::refresh_changed` + уведомление |
| `launch_or_notify_effectcraft(xml)` | сначала loopback, затем запуск `effectcraft.exe`, иначе `saved_only` |
| `find_effectcraft_exe()` | `EFFECTCRAFT_PATH`, рядом с exe, `../EffectCraft/`, `../../EffectCraft/`, `Program Files`, `PATH` |

### 4.3 Команды

| Команда | Меню | Параметры | Результат |
| --- | --- | --- | --- |
| `clip.replaceWithCraftLink` | Clip | `{name?, path?, autoSync?, launch?}` | `{sequence, clips, path, name, start, duration, effectcraft, craftlink, replaced:true}` |
| `clip.sendToCraftLink` | Clip | `{name?, path?, autoSync?, launch?}` | то же, `replaced:false` |
| `craftlink.refresh` | — | `{}` | результат `media.refreshChanged` |

`launch:false` не запускает EffectCraft (тесты, агенты); `autoSync:false` запрещает EffectCraft
перерендеривать мастер при сохранении для этой связи.

Объект `craftlink` в результате:

```json
{"linked": true, "render": "…\\CraftLink\\CraftLink Comp 01.mov", "autoSync": true}
```

`linked` равно `false`, если мастер не удалось прочитать обратно (тогда на таймлайне остаётся
вложенная композиция, которая рендерит то же содержимое), а `error` объясняет причину.

### 4.4 Дополнение наблюдателя медиа

`Session::note_media_stamp(path)` запоминает текущие размер и время изменения файла как
базовый штамп. Вызывается сразу после вставки связанного мастера, потому что наблюдатель
намеренно считает `None → Some` «не изменилось» (файл, которого не было при прошлом взгляде,
не считается изменившимся). Без этой базовой отметки сохранение в EffectCraft, случившееся до
первого планового сканирования FilmCraft, было бы пропущено.

### 4.5 Интерфейс

- Контекстное меню клипа таймлайна: **Replace with EffectCraft Composition**, **Send to EffectCraft**.
- Меню Clip: те же команды.
- `tick_media_refresh` запускает сканирование сразу, как только появился файл `.notify`, даже
  если *Automatically refresh growing files* выключено.
- Проект, показывающий связанный мастер (`media_watch::watches_linked_master`: медиафайл внутри
  папки `CraftLink`), опрашивает spool-папку каждые 0.5 с: живой рендер виден сразу, а не через
  минуту, на следующем плановом сканировании. Плановое сканирование по-прежнему подчиняется
  *Automatically refresh growing files*, а уведомление CraftLink — нет.

---

## 5. Изменения в EffectCraft

### 5.1 Файлы

| Файл | Изменение |
| --- | --- |
| `crates/engine/src/craftlink.rs` | **новый** — манифест, хранение в комментарии, синхронизация, живая связь, уведомление |
| `crates/engine/src/cmd/timeline.rs` | `prerender_cx`, `render_comp`, регистрация при импорте, 3 команды (`file.sendToFilmCraft`, `file.craftlinkSync`, `file.craftlinkStatus`) |
| `crates/engine/src/cmd.rs` | `pub(crate) mod timeline` |
| `crates/engine/src/cmd/file.rs` | `auto_sync` после `save` / `saveAs` |
| `crates/engine/src/lib.rs` | `mod craftlink`, `pub use craftlink::LiveSync`, `Session::craftlink_live` |
| `crates/engine/src/session/dispatch.rs` | `craftlink_note_change` после команды, изменившей `revision` |
| `crates/ui-egui/src/app/frame.rs` | `craftlink_tick(self.now)` в логическом тике |
| `crates/engine/src/menus.rs` | пункты File ▸ Export |
| `crates/engine/src/tests/timeline_interchange.rs` | тест `send_to_filmcraft_exports_xml` |
| `apps/effectcraft/src/main.rs` | файл таймлайна (`.xml`/`.fcpxml`/`.otio`/`.edl`/`.aaf`/`.omf`), переданный в командной строке, открывается как таймлайн; убрано консольное окно |

### 5.2 Функции (`crates/engine/src/craftlink.rs`)

| Функция | Ответственность |
| --- | --- |
| `CraftLinkSource::from_value / to_value` | манифест ⇄ структура |
| `CraftLinkSource::to_comment / from_comment` | хранение в `Item.comment` |
| `manifest_path(xml)` | путь бокового файла |
| `source_for_timeline(xml)` | чтение бокового файла |
| `source_of(s, cid)` | чтение связи из комментария композиции |
| `partial_path(render)` | `foo.mov` → `foo.partial.mov` |
| `notify_filmcraft(render)` | spool-файл + очистка + необязательный loopback |
| `notify_filmcraft_control()` | аутентифицированный JSON-lines запрос |
| `spawn_render(s, cid, src, wait, quiet)` | рендер в `.partial.mov`, переименование поверх мастера, уведомление |
| `sync(s, p)` | `file.craftlinkSync` |
| `status(s, p)` | `file.craftlinkStatus` |
| `auto_sync(s)` | перерендер всех связей с `autoSync` после сохранения |
| `craftlink_note_change(id)` | взвести живую связь после правки (сохранения, открытия и импорт пропускаются) |
| `craftlink_tick(now)` | рендер взведённой связи, когда период тишины истёк |
| `craftlink_live_pending()` | ждёт ли живой рендер (подсказка перерисовки для интерфейса) |
| `register_import(project, comps, xml)` | запись комментария при импорте |

### 5.3 Рендер всей композиции (`crates/engine/src/cmd/timeline.rs`)

`render_comp(cx, project, cid, path, progress)` собирает один `RenderQueueItem`:

```
time_span      = TimeSpan::LengthOfComp
output.format  = OutputFormat::ProRes
channels       = Channels::Rgba
prores_profile = ProResProfile::P4444
audio          = AudioOutput::Auto
output.output  = path
```

и отдаёт его `Exporter` сессии. `prerender_cx(s)` собирает кодировщик, источник футажа, хост
выражений, GPU-ускоритель и настройку *Switches Affect Nested Comps* — так же, как экспорт
таймлайна, поэтому фоновый рендер видит тот же проект, что и интерфейс.

### 5.4 Команды

| Команда | Меню | Параметры | Результат |
| --- | --- | --- | --- |
| `file.craftlinkSync` | File ▸ Export ▸ Update CraftLink Render | `{comp?, background?}` | `{comp, render, xml}` (или `{job}` при `background:true`) |
| `file.craftlinkStatus` | — | `{comp?}` | `{linked, composition, xml, render, autoSync, application, exists, bytes, modified}` |
| `file.sendToFilmCraft` | File ▸ Export ▸ Send to FilmCraft… | `{comp?, path?}` | `{comp, name, path, export, filmcraft}` — `filmcraft.status` равен `connected_to_control_server`, `launched`, `launch_failed` или `saved_only` |

`file.sendToFilmCraft` — обратное направление: экспортирует активную композицию в XML Final Cut Pro,
затем отдаёт файл работающему FilmCraft по control-каналу (`engine.execute` → `file.import`), либо
запускает `filmcraft.exe` на этом файле, либо оставляет `.xml` на диске (`saved_only`).
`find_filmcraft_exe` проверяет `FILMCRAFT_PATH`, файл рядом с exe, `../FilmCraft/`, `../../FilmCraft/`,
`Program Files` и `PATH` — в этом порядке.

### 5.5 Хук сохранения

`crates/engine/src/cmd/file.rs`:

```rust
fn save(s, p)    { let r = save_to(s, &path)?; crate::craftlink::auto_sync(s); Ok(r) }
fn save_as(s, p) { let r = save_to(s, path)?;  crate::craftlink::auto_sync(s); Ok(r) }
```

Сам `save_to` **не** перехватывается, поэтому автосохранение, *Increment and Save* и
восстановление не запускают рендеры. `auto_sync` пропускает запуск, если задача `craftlink`
уже выполняется (один рендер за раз), и учитывает только связи с `autoSync = true`.

### 5.6 Живая связь (без сохранения)

Мастер следует за правками без File ▸ Save:

| Часть | Где | Что делает |
| --- | --- | --- |
| `Session::craftlink_live: LiveSync` | `lib.rs`, `craftlink.rs` | `{dirty, since, seen, comp, rendered}` — состояние дребезга (debounce) |
| `craftlink_note_change(id)` | `session/dispatch.rs` | после любой команды, изменившей `revision`: взводит связь для активной тогда композиции; сохранения, открытия, revert и импорт только помечают ревизию увиденной |
| `craftlink_tick(now)` | `ui-egui/src/app/frame.rs` | когда правок нет `LIVE_DELAY` (1.0 с), запускает один тихий `spawn_render` |
| `craftlink_live_pending()` | `ui-egui/src/app/frame.rs` | держит перерисовку каждые 120 мс, пока рендер должен быть |
| `watches_linked_master(project)` | FilmCraft `media_watch.rs` | проект с медиафайлом в папке `CraftLink` опрашивает spool каждые 0.5 с |

Правила:

- Рендерится только композиция, **активная** в момент правки; переключение композиций посреди
  правки не рендерит другую.
- Один рендер за раз: правка во время рендера запоминается и рендерится после него, а не
  ставится в очередь стопкой.
- `autoSync: false` выключает и живую связь: такую связь рендерят только `file.craftlinkSync` и
  *File ▸ Save*.
- Живой рендер тихий (без тоста); сообщают только явная синхронизация и сохранения.
- У движка нет часов, поэтому дребезг считает интерфейс по своим монотонным `AppClock::now()` и
  передаёт время в `craftlink_tick`.

---

## 6. Исправление пропавшего затемнения (fade-out)

### Симптом

После отправки выделения в EffectCraft у **последнего выбранного клипа** пропадало затемнение
(fade-out) в EffectCraft (прислан скриншот).

### Причина

Переход может пересекать границу выделения: его исходящая сторона выбрана, входящая — нет.
Старый мост копировал переход только если выбраны **обе** стороны, поэтому граничный переход
терялся, и клип лишался затемнения.

### Исправление

`nest_selection_for_bridge` теперь сохраняет каждый переход, у которого хотя бы одна сторона в
выделении, а отсутствующую сторону делает «чёрной»:

| Исходно | Выделение | Результат |
| --- | --- | --- |
| `from = A`, `to = B` | только A | `from = A`, `to = None` → `end-black` (затемнение) |
| `from = A`, `to = B` | только B | `from = None`, `to = B` → `start-black` (появление) |
| `from = None`, `to = B` | B | без изменений → `start-black` |
| `from = A`, `to = None` | A | без изменений → `end-black` |
| обе стороны вне выделения | — | отбрасывается |
| обе стороны стали `None` | — | отбрасывается |

Затем переход **обрезается до границ выбранного клипа** — но только если он действительно
потерял вторую сторону; переход, у которого обе стороны в выделении (кросс-диссолв между двумя
выбранными клипами), копируется целиком:

- затемнение (`from` выбран): `end = min(transition.end, clip.end)`,
  `duration = end - start`;
- появление (`to` выбран): `start = max(transition.start, clip.start)`,
  `duration = end - start`.

Писатель FCP7 выводит `start-black` / `end-black` из пары `(from, to)` (`write_transition`,
`crates/interchange/src/fcp7.rs`). Импортёр EffectCraft уже превращает их в ключи прозрачности
(`crates/interchange/src/import.rs`):

```rust
if tr.to == Some(clip)          { fades.push((start, end, 0.0, 1.0)); } // появление
if tr.from == Some(clip) && tr.to.is_none() { fades.push((start, end, 1.0, 0.0)); } // затемнение
```

Благодаря обрезке затемнение теперь покрывает последние кадры клипа, а не уходит за его конец.

Регрессионный тест: `boundary_transition_becomes_a_one_sided_fade` и
`a_cross_dissolve_between_two_selected_clips_keeps_both_sides`
(`crates/engine/src/craftlink_bridge_tests.rs`).

---

## 7. Последовательность живой синхронизации

1. **FilmCraft** — редактор выделяет клипы, вызывает `clip.replaceWithCraftLink`.
2. `nest_selection_for_bridge` создаёт вложенную последовательность (а для *replace* заменяет
   выделение клипом вложенной последовательности).
3. `materialize_generators_for_export` запекает процедурные демо-генераторы в MP4 (в EffectCraft
   нет генераторов FilmCraft).
4. `filmcraft_interchange::export` пишет `<имя>.xml`.
5. `bake_linked_master` рендерит `CraftLink/<имя>.mov` (ProRes 4444 + альфа + PCM), а
   `write_manifest` пишет `<имя>.craftlink.json`.
6. `link_placed_clips_to_master` добавляет медиа-элемент мастера, перенаправляет на него клипы
   и запоминает штамп.
7. `launch_or_notify_effectcraft` просит запущенный EffectCraft импортировать XML (loopback) или
   запускает `effectcraft.exe <xml>`.
8. **EffectCraft** — `file.importTimeline` импортирует XML, затем `register_import` пишет
   `craftlink:{…}` в комментарий композиции.
9. Редактор добавляет эффекты (Glow, маски…) и сохраняет.
10. `auto_sync` → `spawn_render` → `render_comp` пишет `<имя>.partial.mov`, переименовывает
    поверх `<имя>.mov`, затем `notify_filmcraft` пишет spool-уведомление.
11. **FilmCraft** — `tick_media_refresh` замечает уведомление, запускает сканирование, оно
    перечитывает изменённый мастер, таймлайн перерисовывается с новым рендером.

Задержка: время рендера (всей композиции) плюс один кадр интерфейса. Потоковой передачи по
кадрам нет.

---

## 8. Настройки и значения по умолчанию

| Настройка | Где | По умолчанию |
| --- | --- | --- |
| *Automatically refresh growing files* | FilmCraft ▸ Settings ▸ Media | включено |
| *Refresh growing files every* | FilmCraft ▸ Settings ▸ Media | 60 с |
| `autoSync` | параметр команды / манифест | `true` |
| `launch` | параметр команды | `true` |

Уведомление CraftLink запускает сканирование даже при выключенном автообновлении.

---

## 9. Сборка и тесты

Проект собирается инструментарием `stable-x86_64-pc-windows-gnullvm` (без MSVC `link.exe`).

```powershell
# FilmCraft
cd filmcraft-0.20.1
cargo build --release -p filmcraft
cargo test -p filmcraft-engine craftlink

# EffectCraft
cd effectcraft-0.24.1
cargo build --release -p effectcraft
cargo test -p effectcraft-engine craftlink
cargo test -p effectcraft-engine menus::tests
```

Ожидаемые тесты CraftLink:

```
craftlink_bridge_tests::craftlink_fails_when_no_clips_selected            ok
craftlink_bridge_tests::replace_with_craftlink_creates_xml_and_replaces_on_timeline   ok
craftlink_bridge_tests::send_to_craftlink_keeps_original_timeline         ok
craftlink_bridge_tests::boundary_transition_becomes_a_one_sided_fade      ok
craftlink_bridge_tests::a_cross_dissolve_between_two_selected_clips_keeps_both_sides ok
craftlink_bridge_tests::the_first_bridge_run_creates_its_link_folder         ok
craftlink::tests::a_manifest_round_trips_through_a_comment                ok
craftlink::tests::a_plain_comment_is_not_a_link                           ok
craftlink::tests::the_manifest_sits_next_to_the_timeline                  ok
craftlink::tests::a_partial_render_keeps_the_extension                    ok
craftlink::tests::an_edit_arms_the_live_link                              ok
craftlink::tests::opening_a_document_does_not_arm_the_live_link           ok
craftlink::tests::the_live_link_waits_for_the_quiet_period                ok
craftlink::tests::a_new_edit_restarts_the_quiet_period                    ok
craftlink::tests::an_unlinked_composition_is_not_rendered_by_the_live_link ok
craftlink::tests::a_link_with_auto_sync_off_waits_for_a_save              ok
```

После релизной сборки скопируйте `target/release/filmcraft.exe` и
`target/release/effectcraft.exe` в их portable-папки (предыдущий бинарник сохраните как
`.exe.bak`).

---

## 10. Перенос CraftLink на другую пару приложений

Функции нужны всего четыре возможности, и ни одна не является проприетарной:

1. **Писать таймлайн обмена.** Любой формат, который читают обе стороны (здесь FCP7 XML).
2. **Писать и читать боковой JSON** рядом с ним.
3. **Рендерить композицию в видеофайл**, который декодируют обе стороны (здесь ProRes 4444).
4. **Замечать изменение файла** (наблюдатель, spool-файл или сокет).

Минимальная реализация:

```
send(selection):
    seq = nest(selection)
    xml = export(seq, FCP7)
    write(xml)
    write(xml.stem + ".craftlink.json", {craftlink:1, composition, xml, render, autoSync})
    render(seq, render)
    replace_on_timeline(selection, render)   # необязательно
    open_or_notify(other_app, xml)

import(xml):
    comp = import_timeline(xml)
    link = read(xml.stem + ".craftlink.json")
    if link: remember(comp, link)

save(project):
    for comp in project where remembered(comp).autoSync:
        render(comp, remembered(comp).render)
        notify(remembered(comp).render)

edit(project):                     # after any command that changed the document
    link = remembered(active_comp)
    if link and link.autoSync:     # saves, opens and imports are not edits
        armed = link
        armed.due = now + 1.0      # a new edit restarts the quiet period

tick(now):                         # the frontend's frame loop, monotonic clock
    if armed and now >= armed.due and no render running:
        render(armed.comp, armed.render)
        notify(armed.render)
        armed = none

notify(render):
    write(temp/"CraftLink"/"reload-<pid>-<ms>.notify", {craftlink:1, render})
```

`remember` может быть полем комментария (как здесь), таблицей проекта или базой данных —
формат не важен, пока он сохраняется вместе с проектом.

---

## 11. Известные ограничения

- **Не по каждому нажатию.** Мастер перерендеривается через секунду после последней правки, по
  *File ▸ Save* (или `file.craftlinkSync`), а не на каждый промежуточный кадр движения
  ползунка. Следующий шаг — кеш по кадрам.
- **Рендер всей композиции.** EffectCraft рендерит композицию целиком, а не только изменённую
  область.
- **Абсолютные пути.** Перенос папки проекта разрывает связь, пока не отредактирован манифест.
- **Один рендер за раз.** Правка или сохранение во время идущего рендера CraftLink запоминается;
  его подхватит следующий тик или сохранение.
- **Только активная композиция.** Перерендеривается лишь та связанная композиция, что была
  активна в момент правки: правка другой композиции, использующей её как вложенный прекомп, мастер
  не обновляет.
- **Размер мастера.** ProRes 4444 с альфой занимает много места; папку обмена лучше держать на
  быстром диске.
- **Наличие ProRes.** Обе сборки должны включать кодировщик/декодировщик ProRes (включают).

---

## 12. Локализация

Три новые строки интерфейса FilmCraft — английский источник и 16 каталогов переводов в
`crates/ui-egui/src/i18n/`:

| Исходная строка (английский) | Где |
| --- | --- |
| `Replace with EffectCraft Composition` | меню Clip, контекстное меню таймлайна |
| `Send to EffectCraft…` | меню Clip, контекстное меню таймлайна |
| `Update CraftLink Renders` | палитра команд / реестр агента |

Отсутствующие переводы показываются по-английски, отчёт i18n всегда проходит. Строки
переведены в `ru.tsv`, `uk.tsv`, `de.tsv`, `es.tsv`, `fr.tsv`, `it.tsv`, `pt-br.tsv`,
`pl.tsv`, `cs.tsv`, `sv.tsv`, `tr.tsv`, `ja.tsv`, `ko.tsv`, `zh-cn.tsv`, `id.tsv`, `bs.tsv`.

---

## 13. История изменений

| Дата | Изменение |
| --- | --- |
| 14.11.2026 | CraftLink: переименован из прежнего моста `EffectCraft`; живой мастер, манифест, spool-уведомление, автосинхронизация при сохранении, `craftlink.refresh`, `file.craftlinkSync`, `file.craftlinkStatus` |
| 14.11.2026 | Исправлено пропавшее затемнение в конце выбранного клипа (граничные переходы становятся односторонними затемнениями) |
| 14.11.2026 | `Session::note_media_stamp`, чтобы рендер, записанный до первого планового сканирования, всё равно замечался |
| 16.11.2026 | Обрезка применяется только к переходу, потерявшему вторую сторону; кросс-диссолв между двумя выбранными клипами копируется целиком |
| 16.11.2026 | Пункты меню переименованы в **Replace with EffectCraft Composition** / **Send to EffectCraft…**; живая связь рендерит через 1 с после последней правки, без File ▸ Save (`LiveSync`, `craftlink_tick`); FilmCraft опрашивает spool каждые 0.5 с, пока в проекте есть связь |
| 17.11.2026 | Папка связи создаётся до первой записи, поэтому первый запуск моста больше не падает с `os error 3`; регрессионный тест `the_first_bridge_run_creates_its_link_folder` |
