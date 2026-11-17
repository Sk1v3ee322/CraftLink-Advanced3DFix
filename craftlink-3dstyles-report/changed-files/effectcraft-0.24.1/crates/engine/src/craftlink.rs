//! CraftLink: the receiving end of the live composition link with FilmCraft.
//!
//! A timeline sent from FilmCraft arrives as an ordinary Final Cut Pro 7 XML file, with a
//! sidecar manifest next to it (`<name>.craftlink.json`) that names the composition, the
//! timeline file and the rendered master FilmCraft shows. This module:
//!
//! - reads that manifest and remembers it on the imported composition (`Item.comment`), so the
//!   link survives saving and reopening the project;
//! - re-renders the composition to the linked master on demand (`file.craftlinkSync`) and, when
//!   the manifest asks for it, after every File ▸ Save (`auto_sync`);
//! - tells FilmCraft that the master changed with a small spool file in the temp folder (and,
//!   when it is running with a control token, a loopback request), so the FilmCraft timeline
//!   updates at once.
//!
//! Everything is an open format: Final Cut Pro 7 XML (`<xmeml>`), Apple ProRes 4444 in
//! QuickTime, a JSON sidecar and a JSON-lines loopback request. No proprietary protocol or
//! shared library is involved, so the behaviour can be reimplemented from this description.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use effectcraft_project::ItemId;
use serde_json::{Value, json};

use crate::cmd::{b_p, bad, comp_id};
use crate::jobs::Apply;
use crate::{Result, Session};

/// Version of the sidecar manifest this build reads and writes.
pub const MANIFEST_VERSION: u32 = 1;
/// Folder (in the temp folder) FilmCraft polls for "a linked master changed" notices.
pub const LINK_FOLDER: &str = "CraftLink";
/// Suffix of the sidecar manifest next to the timeline file.
pub const MANIFEST_SUFFIX: &str = ".craftlink.json";
/// Marks the JSON line stored in `Item.comment`.
pub const COMMENT_PREFIX: &str = "craftlink:";
/// How long the live link waits after the last edit before re-rendering, in seconds: long enough
/// to fold a slider drag or a burst of keystrokes into one render, short enough to feel immediate.
pub const LIVE_DELAY: f64 = 1.0;

/// Commands that change the document without being an edit of a linked composition: opening,
/// creating, reverting or importing a project, and the link's own commands. They must not arm the
/// live render (a freshly opened project would otherwise render on top of its own master).
const LIVE_IGNORED: &[&str] = &[
    "file.save",
    "file.saveAs",
    "file.saveCopy",
    "file.saveCopyAsXml",
    "file.craftlinkSync",
    "file.open",
    "file.openDemoProject",
    "file.newProject",
    "file.revert",
    "file.openRecent",
    "file.importTimeline",
    "file.import",
    "file.importProject",
    "file.importAep",
    "file.importMultiple",
];

/// The debounced live link: the active linked composition re-renders its master shortly after an
/// edit, without waiting for File ▸ Save.
///
/// The engine has no clock, so [`Session::craftlink_note_change`] only records that something
/// changed and [`Session::craftlink_tick`] — called from the frontend's frame loop with its
/// monotonic clock — decides when the quiet period is over.
#[derive(Clone, Debug, Default)]
pub struct LiveSync {
    /// An edit is waiting for the quiet period to expire.
    dirty: bool,
    /// When the quiet period started (`None`: the next tick starts it).
    since: Option<f64>,
    /// The last project revision handled: a change counts once.
    seen: u64,
    /// The linked composition that was active when the change was recorded.
    comp: Option<ItemId>,
    /// Renders the live link started (tests and `file.craftlinkStatus`).
    pub rendered: u64,
}

/// One CraftLink, as written by FilmCraft in the sidecar manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CraftLinkSource {
    pub version: u32,
    /// The composition's name in FilmCraft.
    pub composition: String,
    /// The Final Cut Pro 7 XML timeline that was imported.
    pub xml: String,
    /// The ProRes 4444 master FilmCraft shows and this composition renders to.
    pub render: String,
    /// Re-render the master after every save.
    pub auto_sync: bool,
    /// The application that created the link ("FilmCraft").
    pub application: String,
}

impl CraftLinkSource {
    /// Reads a manifest value (`{craftlink, composition, xml, render, autoSync, application}`).
    pub fn from_value(v: &Value) -> Option<Self> {
        let render = v.get("render").and_then(Value::as_str)?.to_string();
        if render.trim().is_empty() {
            return None;
        }
        Some(Self {
            version: v.get("craftlink").and_then(Value::as_u64).unwrap_or(MANIFEST_VERSION as u64) as u32,
            composition: v.get("composition").and_then(Value::as_str).unwrap_or_default().to_string(),
            xml: v.get("xml").and_then(Value::as_str).unwrap_or_default().to_string(),
            render,
            auto_sync: v.get("autoSync").and_then(Value::as_bool).unwrap_or(true),
            application: v.get("application").and_then(Value::as_str).unwrap_or("FilmCraft").to_string(),
        })
    }

    /// The manifest value.
    pub fn to_value(&self) -> Value {
        json!({
            "craftlink": self.version,
            "composition": self.composition,
            "xml": self.xml,
            "render": self.render,
            "autoSync": self.auto_sync,
            "application": self.application,
        })
    }

    /// The line stored in `Item.comment` so the link survives a project reload.
    pub fn to_comment(&self) -> String {
        format!("{COMMENT_PREFIX}{}", self.to_value())
    }

    /// The link stored in an item's comment, if the comment is one.
    pub fn from_comment(comment: &str) -> Option<Self> {
        let rest = comment.strip_prefix(COMMENT_PREFIX)?;
        let v: Value = serde_json::from_str(rest.trim()).ok()?;
        Self::from_value(&v)
    }
}

/// The sidecar manifest that belongs to a timeline file.
pub fn manifest_path(xml: &Path) -> PathBuf {
    let mut name = xml.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "CraftLink".into());
    name.push_str(MANIFEST_SUFFIX);
    xml.with_file_name(name)
}

/// Reads the manifest next to `xml`, if there is one and it names a master.
pub fn source_for_timeline(xml: &Path) -> Option<CraftLinkSource> {
    let text = std::fs::read_to_string(manifest_path(xml)).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    CraftLinkSource::from_value(&v)
}

/// The link a composition was imported with, from its item comment.
pub fn source_of(s: &Session, cid: ItemId) -> Option<CraftLinkSource> {
    CraftLinkSource::from_comment(&s.project.item(cid)?.comment)
}

/// A temporary name next to the master, so a failed render never leaves a half-written file
/// where FilmCraft reads it.
fn partial_path(render: &str) -> String {
    let p = Path::new(render);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "master".into());
    let name = match p.extension().map(|s| s.to_string_lossy().to_string()) {
        Some(e) => format!("{stem}.partial.{e}"),
        None => format!("{stem}.partial"),
    };
    p.with_file_name(name).to_string_lossy().to_string()
}

/// Tells FilmCraft that a linked master changed: a spool file it polls while it runs, plus a
/// best-effort loopback request when it was started with a control token.
pub fn notify_filmcraft(render: &str) {
    let dir = std::env::temp_dir().join(LINK_FOLDER);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let name = format!("reload-{}-{stamp}.notify", std::process::id());
    let body = json!({"craftlink": MANIFEST_VERSION, "render": render}).to_string();
    let _ = std::fs::write(dir.join(name), body);
    // A FilmCraft that is not running leaves the spool behind: drop what is old.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let now = SystemTime::now();
        for e in entries.flatten() {
            let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|d| d.as_secs() > 600);
            if old {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = notify_filmcraft_control();
}

/// `craftlink.refresh` on a running FilmCraft, authenticated with the control token the process
/// inherited (`FILMCRAFT_CONTROL_TOKEN`, 64 hex characters). FilmCraft keeps its listener closed
/// without that token, so the spool file above remains the only notification it needs.
fn notify_filmcraft_control() -> std::result::Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let token = std::env::var("FILMCRAFT_CONTROL_TOKEN").ok().filter(|t| t.len() == 64).ok_or("no control token")?;

    let mut ports = vec![9876];
    for file in [PathBuf::from("filmcraft_control.json"), std::env::temp_dir().join("filmcraft_control.json")] {
        // A missing or malformed discovery file just means "use the default port".
        let Ok(content) = std::fs::read_to_string(file) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&content) else { continue };
        if let Some(p) = v.get("port").and_then(Value::as_u64) {
            ports.insert(0, p as u16);
        }
    }

    for port in ports {
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(150)) else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let req = json!({
            "token": token,
            "method": "engine.execute",
            "params": {"command": "craftlink.refresh", "params": {}},
        });
        if writeln!(stream, "{req}").is_err() {
            continue;
        }
        let mut resp = String::new();
        if BufReader::new(stream).read_line(&mut resp).is_ok() && !resp.trim().is_empty() {
            return Ok(());
        }
    }
    Err("FilmCraft control channel unavailable".into())
}

/// Renders `cid` to its linked master in the background (`wait` renders it before returning).
/// `quiet` leaves the "updated" toast out: the live link renders often and must not chatter.
fn spawn_render(s: &mut Session, cid: ItemId, src: &CraftLinkSource, wait: bool, quiet: bool) -> Result<Value> {
    let cx = crate::cmd::timeline::prerender_cx(s);
    let project = s.project.clone();
    let (render, xml, composition) = (src.render.clone(), src.xml.clone(), src.composition.clone());
    let label = if composition.is_empty() { "CraftLink".to_string() } else { format!("CraftLink {composition}") };
    s.spawn_task("craftlink", label, wait, move |ctl| {
        let partial = partial_path(&render);
        if let Some(parent) = Path::new(&partial).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut progress = |_: u64, _: u64| !ctl.cancelled();
        crate::cmd::timeline::render_comp(&cx, &project, cid, &partial, &mut progress)?;
        // The master is replaced only once the whole render is written: FilmCraft may be
        // reading the old one right now.
        std::fs::rename(&partial, &render).map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            format!("cannot replace {render}: {e}")
        })?;
        notify_filmcraft(&render);
        let done = render.clone();
        Ok(Box::new(move |s: &mut Session| {
            if !quiet {
                s.toast(format!("CraftLink: updated {}", done));
            }
            Ok(json!({"comp": cid.0, "render": done, "xml": xml}))
        }) as Apply)
    })
}

/// `file.craftlinkSync`: re-render the linked master now.
pub fn sync(s: &mut Session, p: &Value) -> Result<Value> {
    let cid = comp_id(s, p)?;
    let src = source_of(s, cid).ok_or_else(|| {
        bad(
            "file.craftlinkSync",
            "this composition has no CraftLink: import a timeline sent from FilmCraft (Clip ▸ Replace with EffectCraft Composition) first",
        )
    })?;
    let wait = !b_p(p, "background").unwrap_or(false);
    spawn_render(s, cid, &src, wait, false)
}

/// `file.craftlinkStatus`: what the composition is linked to and whether its master exists.
pub fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let cid = comp_id(s, p)?;
    let name = s.project.item(cid).map(|i| i.name.clone()).unwrap_or_default();
    let Some(src) = source_of(s, cid) else {
        return Ok(json!({"comp": cid.0, "name": name, "linked": false}));
    };
    let meta = std::fs::metadata(&src.render).ok();
    Ok(json!({
        "comp": cid.0,
        "name": name,
        "linked": true,
        "composition": src.composition,
        "xml": src.xml,
        "render": src.render,
        "autoSync": src.auto_sync,
        "application": src.application,
        "exists": meta.is_some(),
        "bytes": meta.as_ref().map(|m| m.len()),
        "modified": meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_millis()),
    }))
}

/// After File ▸ Save / Save As: re-render every linked master whose manifest asks for it.
///
/// One render at a time: a save while a CraftLink render is still running leaves the next one to
/// the following save rather than stacking jobs.
pub fn auto_sync(s: &mut Session) {
    if s.tasks.iter().any(|t| t.kind == "craftlink") {
        return;
    }
    let jobs: Vec<(ItemId, CraftLinkSource)> =
        s.project.items.values().filter_map(|i| CraftLinkSource::from_comment(&i.comment).map(|src| (i.id, src))).filter(|(_, src)| src.auto_sync).collect();
    for (cid, src) in jobs {
        let _ = spawn_render(s, cid, &src, false, false);
    }
}

impl Session {
    /// A command changed the project: remember that the active linked composition needs a fresh
    /// master. [`Session::craftlink_tick`] starts the render once the edits settle.
    ///
    /// Called by [`Session::execute`] for every command that advanced [`Session::revision`].
    pub fn craftlink_note_change(&mut self, id: &str) {
        if self.revision == self.craftlink_live.seen {
            return;
        }
        if LIVE_IGNORED.contains(&id) {
            // The document was replaced rather than edited: don't render the newcomer on top of
            // the master it was opened from.
            self.craftlink_live.seen = self.revision;
            return;
        }
        self.craftlink_live.dirty = true;
        // The next tick starts the quiet period again: the last edit wins.
        self.craftlink_live.since = None;
        self.craftlink_live.comp = self.active_comp_id();
    }

    /// The live link is waiting for its quiet period or its render.
    pub fn craftlink_live_pending(&self) -> bool {
        self.craftlink_live.dirty
    }

    /// Frontend frame work for the live link: re-render the linked composition that was active
    /// when the edit happened, once no edit arrived for [`LIVE_DELAY`] seconds. One render at a
    /// time: an edit during a render waits for it to finish.
    ///
    /// `now` is the frontend's monotonic clock in seconds (the engine has none).
    pub fn craftlink_tick(&mut self, now: f64) {
        if !self.craftlink_live.dirty || self.tasks.iter().any(|t| t.kind == "craftlink") {
            return;
        }
        let Some(cid) = self.craftlink_live.comp.filter(|c| self.project.comp(*c).is_some()) else {
            self.craftlink_live.dirty = false;
            return;
        };
        let Some(src) = source_of(self, cid) else {
            // The composition was unlinked (or is not linked at all): nothing to render.
            self.craftlink_live.dirty = false;
            return;
        };
        if !src.auto_sync {
            // The link was imported with automatic syncing off: only File ▸ Save renders it.
            self.craftlink_live.dirty = false;
            self.craftlink_live.seen = self.revision;
            return;
        }
        let since = match self.craftlink_live.since {
            Some(t) => t,
            None => {
                self.craftlink_live.since = Some(now);
                now
            }
        };
        if now - since < LIVE_DELAY {
            return;
        }
        self.craftlink_live.dirty = false;
        self.craftlink_live.since = None;
        self.craftlink_live.seen = self.revision;
        self.craftlink_live.rendered += 1;
        let _ = spawn_render(self, cid, &src, false, true);
    }
}

/// Remembers the link on the composition that was just imported from a CraftLink timeline.
pub fn register_import(project: &mut effectcraft_project::Project, comps: &[ItemId], xml: &Path) {
    let Some(src) = source_for_timeline(xml) else { return };
    let comment = src.to_comment();
    for cid in comps {
        if let Some(item) = project.item_mut(*cid) {
            item.comment = comment.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_round_trips_through_a_comment() {
        let src = CraftLinkSource {
            version: MANIFEST_VERSION,
            composition: "CraftLink Comp 01".into(),
            xml: "C:\\p\\CraftLink Comp 01.xml".into(),
            render: "C:\\p\\CraftLink\\CraftLink Comp 01.mov".into(),
            auto_sync: true,
            application: "FilmCraft".into(),
        };
        let back = CraftLinkSource::from_comment(&src.to_comment()).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn a_plain_comment_is_not_a_link() {
        assert!(CraftLinkSource::from_comment("a note about this comp").is_none());
        assert!(CraftLinkSource::from_comment("craftlink:not json").is_none());
    }

    #[test]
    fn the_manifest_sits_next_to_the_timeline() {
        let p = manifest_path(Path::new("C:\\p\\Comp.xml"));
        assert_eq!(p, PathBuf::from("C:\\p\\Comp.craftlink.json"));
    }

    #[test]
    fn a_partial_render_keeps_the_extension() {
        assert_eq!(partial_path("C:\\p\\Comp.mov"), "C:\\p\\Comp.partial.mov");
        assert_eq!(partial_path("C:\\p\\Comp"), "C:\\p\\Comp.partial");
    }

    // ---------------------------------------------------------------- live link

    fn demo() -> Session {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s
    }

    /// Give the demo's active composition a link, as an import from FilmCraft would.
    fn link(s: &mut Session, cid: ItemId, auto_sync: bool) {
        let src = CraftLinkSource {
            version: MANIFEST_VERSION,
            composition: "CraftLink Comp 01".into(),
            xml: "C:\\p\\CraftLink Comp 01.xml".into(),
            render: "C:\\p\\CraftLink\\CraftLink Comp 01.mov".into(),
            auto_sync,
            application: "FilmCraft".into(),
        };
        std::sync::Arc::make_mut(&mut s.project).item_mut(cid).unwrap().comment = src.to_comment();
    }

    #[test]
    fn an_edit_arms_the_live_link() {
        let mut s = demo();
        s.bump();
        s.craftlink_note_change("prop.set");
        assert!(s.craftlink_live_pending(), "an edit of the project arms the live render");
    }

    #[test]
    fn opening_a_document_does_not_arm_the_live_link() {
        let mut s = demo();
        s.bump();
        s.craftlink_note_change("file.open");
        assert!(!s.craftlink_live_pending(), "a replaced document is not an edit to render");
    }

    #[test]
    fn the_live_link_waits_for_the_quiet_period() {
        let mut s = demo();
        let cid = s.active_comp_id().expect("the demo opens a composition");
        link(&mut s, cid, true);
        s.bump();
        s.craftlink_note_change("prop.set");
        s.craftlink_tick(10.0);
        s.craftlink_tick(10.0 + LIVE_DELAY / 2.0);
        assert_eq!(s.craftlink_live.rendered, 0, "nothing renders before the quiet period ends");
        assert!(s.craftlink_live_pending(), "the render is still owed");
    }

    #[test]
    fn a_new_edit_restarts_the_quiet_period() {
        let mut s = demo();
        let cid = s.active_comp_id().expect("the demo opens a composition");
        link(&mut s, cid, true);
        s.bump();
        s.craftlink_note_change("prop.set");
        s.craftlink_tick(10.0);
        // Another edit 0.6 s later: the render waits for a full quiet second after it.
        s.bump();
        s.craftlink_note_change("prop.set");
        s.craftlink_tick(10.6);
        s.craftlink_tick(10.6 + LIVE_DELAY / 2.0);
        assert_eq!(s.craftlink_live.rendered, 0);
        assert!(s.craftlink_live_pending());
    }

    #[test]
    fn an_unlinked_composition_is_not_rendered_by_the_live_link() {
        let mut s = demo();
        s.bump();
        s.craftlink_note_change("prop.set");
        s.craftlink_tick(0.0);
        s.craftlink_tick(LIVE_DELAY + 1.0);
        assert_eq!(s.craftlink_live.rendered, 0);
        assert!(!s.craftlink_live_pending(), "an unlinked composition leaves no debt");
    }

    #[test]
    fn a_link_with_auto_sync_off_waits_for_a_save() {
        let mut s = demo();
        let cid = s.active_comp_id().expect("the demo opens a composition");
        link(&mut s, cid, false);
        s.bump();
        s.craftlink_note_change("prop.set");
        s.craftlink_tick(0.0);
        s.craftlink_tick(LIVE_DELAY + 1.0);
        assert_eq!(s.craftlink_live.rendered, 0, "autoSync off renders only on File ▸ Save");
        assert!(!s.craftlink_live_pending());
    }
}
