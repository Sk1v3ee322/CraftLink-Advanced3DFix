//! CraftLink: the live composition link between FilmCraft and EffectCraft.
//!
//! CraftLink is the Craft family's own implementation of the "edit here, see it there" workflow:
//! selected timeline episodes are handed to EffectCraft as a nested composition and come back as
//! a rendered master that stays linked to the composition. Nothing here uses a proprietary
//! format or a private protocol:
//!
//! - **Exchange format:** Final Cut Pro 7 XML (`<xmeml>`), an open, documented interchange
//!   format that both applications already read and write.
//! - **Linked master:** Apple ProRes 4444 in QuickTime (with alpha and PCM audio), an openly
//!   documented codec/container pair that both applications decode.
//! - **Live refresh:** a sidecar manifest (`<name>.craftlink.json`) names the composition and the
//!   master, and a tiny spool file in the temp folder tells FilmCraft a master changed. FilmCraft
//!   re-reads the file it already watches ([`crate::media_watch`]); no socket, token or shared
//!   library is required for the link to work.
//!
//! The commands are `clip.replaceWithCraftLink` (replace the selection with the linked master and
//! send the nested composition to EffectCraft) and `clip.sendToCraftLink` (send without changing
//! the timeline). `craftlink.refresh` re-reads every linked master on demand.
//!
//! Everything is written from the plain behaviour of the two applications: no third-party
//! application, its files, its protocol or its user interface was consulted, so the module can be
//! reimplemented in a clean room from this description alone.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use filmcraft_media::MediaSource;
use filmcraft_project::{ClipId, ItemId, TrackId, TrackKind, resolve_auto_points};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, commands};

/// Version of the sidecar manifest this build writes and reads.
pub const MANIFEST_VERSION: u32 = 1;
/// Folder (under the project's folder, or the temp folder without one) holding linked masters
/// and their manifests.
pub const LINK_FOLDER: &str = "CraftLink";
/// Suffix of the sidecar manifest next to the exported timeline.
pub const MANIFEST_SUFFIX: &str = ".craftlink.json";

/// First unused name for the CraftLink composition sequence.
pub fn next_craftlink_comp_name(s: &Session) -> String {
    (1..=s.project.items.len().saturating_add(100))
        .map(|k| format!("CraftLink Comp {k:02}"))
        .find(|n| !s.project.items.values().any(|i| &i.name == n))
        .unwrap_or_else(|| "CraftLink Comp 01".to_string())
}

/// The folder linked masters and manifests live in: `<project folder>/CraftLink`, or a
/// `CraftLink` folder in the temp folder when the project has not been saved yet.
pub fn link_folder(s: &Session) -> PathBuf {
    s.path
        .as_ref()
        .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(std::env::temp_dir)
        .join(LINK_FOLDER)
}

/// The master file a linked composition renders to.
pub fn master_path(s: &Session, name: &str) -> PathBuf {
    link_folder(s).join(format!("{}.mov", file_stem(name)))
}

/// The sidecar manifest that names the composition and its master.
pub fn manifest_path(xml: &Path) -> PathBuf {
    let mut name = xml.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "CraftLink".into());
    name.push_str(MANIFEST_SUFFIX);
    xml.with_file_name(name)
}

/// A name safe to use as a file name (keeps letters, digits, `-`, `_` and spaces).
fn file_stem(name: &str) -> String {
    let t: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect();
    let t = t.trim().trim_matches('.').to_string();
    if t.is_empty() { "CraftLink".into() } else { t }
}

/// Helper that creates a sequence containing the selected clips (shifted to start at tick 0).
/// If `replace_on_timeline` is true, replaces the selected clips on the active sequence with
/// the new nested sequence (like `nest`).
///
/// Transitions whose other side is outside the selection survive as one-sided fades
/// (`start-black` / `end-black`), trimmed to the selected clip, so a fade-out at the end of the
/// last selected clip is not lost when only one side of the cut is sent.
pub fn nest_selection_for_bridge(s: &mut Session, name: &str, replace_on_timeline: bool) -> Result<(ItemId, Vec<ClipId>, TimeRange)> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let sel = commands::with_links(s, &s.state.selection.clone());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let mut items: Vec<(TrackKind, usize, filmcraft_project::TrackItem)> = Vec::new();
    let mut transitions: Vec<(TrackKind, usize, filmcraft_project::Transition)> = Vec::new();

    for (k, ts) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        for (ti, t) in ts.iter().enumerate().filter(|(_, t)| !t.locked) {
            for i in t.items.iter().filter(|i| sel.contains(&i.id)) {
                items.push((k, ti, i.clone()));
            }
            let nested = |c: Option<ClipId>| c.is_none_or(|c| sel.contains(&c));
            for trn in t.transitions.iter().filter(|x| x.from.is_some() || x.to.is_some()) {
                let (from_in, to_in) = (nested(trn.from), nested(trn.to));
                // Neither side is part of the exchange: the transition has nothing to fade.
                if !from_in && !to_in {
                    continue;
                }
                let mut trn = trn.clone();
                // The side outside the selection becomes black: the FCP7 writer turns that into
                // `start-black` / `end-black`, which EffectCraft imports as a fade in / fade out.
                if !from_in {
                    trn.from = None;
                }
                if !to_in {
                    trn.to = None;
                }
                // A transition whose two sides are both outside the selection (or black on both
                // sides) has nothing left to fade.
                if trn.from.is_none() && trn.to.is_none() {
                    continue;
                }
                let bounds = |c: Option<ClipId>| c.and_then(|c| items.iter().find(|i| i.2.id == c).map(|i| (i.2.start, i.2.end())));
                // Trim only a transition that lost its other side to black. A transition with
                // both sides in the selection (a cross dissolve between two selected clips) is
                // copied whole.
                if !to_in
                    && trn.from.is_some()
                    && let Some((_, clip_end)) = bounds(trn.from)
                    && clip_end < trn.end()
                {
                    // A fade-out belongs to the outgoing clip's own last frames.
                    let end = clip_end.max(trn.start);
                    if end > trn.start {
                        trn.duration = end - trn.start;
                    }
                }
                if !from_in
                    && trn.to.is_some()
                    && let Some((clip_start, _)) = bounds(trn.to)
                    && clip_start > trn.start
                {
                    // A fade-in belongs to the incoming clip's own first frames.
                    let end = trn.end();
                    if end > clip_start {
                        trn.start = clip_start;
                        trn.duration = end - clip_start;
                    }
                }
                transitions.push((k, ti, trn));
            }
        }
    }

    if items.is_empty() {
        return Err(EngineError::Other("select clips on the timeline to send to EffectCraft".into()));
    }

    let start = items.iter().map(|i| i.2.start).min().unwrap_or_default();
    let end = items.iter().map(|i| i.2.end()).max().unwrap_or_default();
    let span = TimeRange::new(start, end - start);

    let tracks_of = |kind: TrackKind| -> Vec<usize> {
        let mut v: Vec<usize> = items.iter().filter(|i| i.0 == kind).map(|i| i.1).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let (video_tracks, audio_tracks) = (tracks_of(TrackKind::Video), tracks_of(TrackKind::Audio));
    let sound_blocked = |ai: usize| q.audio_tracks.get(ai).is_none_or(|t| t.items.iter().any(|i| !sel.contains(&i.id) && i.range().overlaps(&span)));
    let video_only = !video_tracks.is_empty() && (audio_tracks.len() > 1 || audio_tracks.first().is_some_and(|ai| sound_blocked(*ai)));
    let (lowest_v, lowest_a) = (video_tracks.first().copied(), audio_tracks.first().copied().filter(|_| !video_only));

    let nv = video_tracks.last().map_or(1, |i| i + 1);
    let audio_at: HashMap<usize, usize> =
        audio_tracks.iter().filter(|i| **i != 0).enumerate().map(|(n, i)| (*i, n + 1)).chain(std::iter::once((0, 0))).collect();
    let na = audio_at.values().max().map_or(1, |i| i + 1);
    let nested_track = |k: TrackKind, ti: usize| if k == TrackKind::Audio { audio_at.get(&ti).copied().unwrap_or(ti) } else { ti };

    let removed: Vec<ClipId> = items.iter().filter(|i| !(video_only && i.0 == TrackKind::Audio)).map(|i| i.2.id).collect();
    let stays = |k: TrackKind| video_only && k == TrackKind::Audio;

    let edit_label = if replace_on_timeline { "Replace with EffectCraft Composition" } else { "Send to EffectCraft" };
    let (nid, placed_clips) = s.edit(edit_label, |pr, st| {
        let nid = pr.new_sequence(name, q.settings.clone(), nv, na, None);
        let fresh_clips: HashMap<ClipId, ClipId> = items.iter().filter(|i| stays(i.0)).map(|i| (i.2.id, ClipId(pr.alloc_id()))).collect();
        let fresh_transitions: Vec<Option<u64>> = transitions.iter().map(|t| stays(t.0).then(|| pr.alloc_id())).collect();
        let copy = |c: ClipId| fresh_clips.get(&c).copied().unwrap_or(c);

        {
            let nq = pr.sequence_mut(nid).ok_or(EngineError::NoSequence)?;
            for (tr, src) in nq.video_tracks.iter_mut().zip(&q.video_tracks) {
                tr.name = src.name.clone();
            }
            for (from, to) in &audio_at {
                if let (Some(tr), Some(src)) = (nq.audio_tracks.get_mut(*to), q.audio_tracks.get(*from)) {
                    tr.channels = src.channels;
                    if from == to {
                        tr.name = src.name.clone();
                    }
                }
            }
            for (k, ti, it) in &items {
                let mut it = it.clone();
                it.id = copy(it.id);
                it.start -= start;
                nq.tracks_mut(*k).get_mut(nested_track(*k, *ti)).ok_or(EngineError::NoSequence)?.items.push(it);
            }
            for ((k, ti, trn), fresh) in transitions.iter().zip(&fresh_transitions) {
                let mut trn = trn.clone();
                if let Some(id) = fresh {
                    trn.id = filmcraft_project::TransitionId(*id);
                }
                (trn.from, trn.to) = (trn.from.map(copy), trn.to.map(copy));
                trn.start -= start;
                nq.tracks_mut(*k).get_mut(nested_track(*k, *ti)).ok_or(EngineError::NoSequence)?.transitions.push(trn);
            }
            for t in nq.all_tracks_mut() {
                t.sort();
            }
            nq.check().map_err(EngineError::Other)?;
        }

        let mut ids = Vec::new();
        if replace_on_timeline {
            let rate = q.settings.frame_rate;
            let whole = TimeRange::new(Tick::ZERO, span.duration);
            let mut v = pr
                .make_track_item(nid, TrackKind::Video, start, whole, rate)
                .ok_or_else(|| EngineError::Other("cannot place the nested composition".into()))?;
            for e in &mut v.effects {
                resolve_auto_points(e, (q.settings.width, q.settings.height), (q.settings.width, q.settings.height));
            }
            let a = pr
                .make_track_item(nid, TrackKind::Audio, start, whole, rate)
                .ok_or_else(|| EngineError::Other("cannot place the nested composition".into()))?;
            let (link, new_video, new_audio) = (pr.alloc_id(), pr.alloc_id(), pr.alloc_id());
            let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
            filmcraft_edit::delete_items(seq, &removed);

            if video_only {
                let links: Vec<u64> = items.iter().filter(|i| i.0 == TrackKind::Video).filter_map(|i| i.2.link).collect();
                for it in seq.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                    if it.link.is_some_and(|l| links.contains(&l)) {
                        it.link = None;
                    }
                }
            }

            let highest_sound = audio_tracks.last().map_or(0, |i| i + 1);
            for (kind, lowest, mut clip, new_track) in [(TrackKind::Video, lowest_v, v, new_video), (TrackKind::Audio, lowest_a, a, new_audio)] {
                let Some(lowest) = lowest else { continue };
                let tracks = seq.tracks_mut(kind);
                let clear = |i: usize| tracks.get(i).is_some_and(|t| !t.locked && filmcraft_edit::track_range_empty(t, span));
                let selection_clear =
                    |t: usize| kind != TrackKind::Video || video_tracks.iter().filter(|i| **i >= t && **i + 1 < highest_sound).all(|i| clear(*i));
                let free = (lowest..tracks.len()).find(|t| clear(*t) && selection_clear(*t));
                let at = match free {
                    Some(i) => i,
                    None => {
                        let (word, n) = (if kind == TrackKind::Video { "Video" } else { "Audio" }, tracks.len() + 1);
                        tracks.push(filmcraft_project::Track::new(TrackId(new_track), kind, format!("{word} {n}")));
                        tracks.len() - 1
                    }
                };
                clip.link = (lowest_v.is_some() && lowest_a.is_some()).then_some(link);
                ids.push(clip.id);
                let t = tracks.get_mut(at).ok_or(EngineError::NoSequence)?;
                t.items.push(clip);
                t.sort();
            }
            seq.check().map_err(EngineError::Other)?;
            st.selection.clear();
        }

        st.project_selection = vec![nid];
        Ok((nid, ids))
    })?;

    Ok((nid, placed_clips, span))
}

/// Renders a procedural demo scene generator to a real MP4 movie file so EffectCraft can decode and display it as footage.
fn bake_demo_scene(scene: filmcraft_media::DemoScene, dur: Tick) -> std::result::Result<PathBuf, String> {
    let target = std::env::temp_dir().join(format!("FilmCraft_Demo_{scene:?}.mp4"));
    if target.is_file() {
        return Ok(target);
    }
    let w = 640;
    let h = 360;
    let rate = filmcraft_time::FrameRate::FPS_23_976;
    let duration = dur.clamp(Tick::from_seconds_f64(1.0), Tick::from_seconds_f64(4.0));
    let src = filmcraft_media::generators::GeneratorSource::new(filmcraft_media::Generator::Demo(scene), w, h, rate, duration);
    let info = src.info().clone();
    let mut p = filmcraft_project::Project::new("bake_demo");
    let clip = filmcraft_project::MediaClip {
        media: filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::Demo(scene)),
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let gen_item = p.add_item("scene", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Media(Box::new(clip)), None);
    let seq = p.new_sequence("s", filmcraft_project::SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let r = TimeRange::new(Tick::ZERO, duration);
    let mut v = p.make_track_item(gen_item, TrackKind::Video, Tick::ZERO, r, rate).ok_or("cannot make demo video track item")?;
    for e in &mut v.effects {
        filmcraft_project::resolve_auto_points(e, (w, h), (w, h));
    }
    let a = p.make_track_item(gen_item, TrackKind::Audio, Tick::ZERO, r, rate).ok_or("cannot make demo audio track item")?;
    let q = p.sequence_mut(seq).ok_or("no sequence")?;
    q.video_tracks[0].items.push(v);
    q.audio_tracks[0].items.push(a);

    let settings = filmcraft_export::ExportSettings {
        format: filmcraft_export::Format::H264,
        path: target.to_string_lossy().into_owned(),
        bitrate_kbps: 2000,
        ..Default::default()
    };
    let shared: filmcraft_media::SharedSource = Arc::new(src);
    let provider = move |id: ItemId| (id == gen_item).then(|| shared.clone());
    filmcraft_export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).map_err(|e| e.to_string())?;

    Ok(target)
}

/// Materializes any demo procedural generators in sequence `nid` into real MP4 files.
fn materialize_generators_for_export(s: &mut Session, nid: ItemId) -> Result<()> {
    let mut to_bake: Vec<(ItemId, String, filmcraft_project::Label, filmcraft_media::DemoScene, Tick)> = Vec::new();
    if let Some(seq) = s.project.sequence(nid) {
        for t in seq.all_tracks() {
            for it in &t.items {
                if let Some(item) = s.project.item(it.item)
                    && let filmcraft_project::ItemKind::Media(m) = &item.kind
                    && let filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::Demo(scene)) = &m.media
                    && !to_bake.iter().any(|(id, ..)| *id == it.item)
                {
                    to_bake.push((it.item, item.name.clone(), item.label, *scene, m.info.duration));
                }
            }
        }
    }

    if to_bake.is_empty() {
        return Ok(());
    }

    let mut baked_items: Vec<(ItemId, String, filmcraft_project::Label, PathBuf)> = Vec::new();
    for (orig_id, name, label, scene, dur) in to_bake {
        if let Ok(path) = bake_demo_scene(scene, dur) {
            baked_items.push((orig_id, name, label, path));
        }
    }

    if baked_items.is_empty() {
        return Ok(());
    }

    s.edit("Bake Demo Footage", |pr, _| {
        let mut replacements = HashMap::new();
        for (orig_id, name, label, path) in baked_items {
            let file_info = filmcraft_media::MediaInfo {
                name: format!("{name}.mp4"),
                kind: filmcraft_media::MediaKind::Movie,
                duration: Tick::from_seconds_f64(30.0),
                video: Some(filmcraft_media::VideoStreamInfo {
                    width: 1920,
                    height: 1080,
                    frame_rate: filmcraft_time::FrameRate::FPS_23_976,
                    par: (1, 1),
                    codec: "h264".into(),
                    pixel_format: "yuv420p".into(),
                    color: Default::default(),
                    color_assumed: None,
                    has_alpha: false,
                    bitrate: Some(6000),
                    hdr: None,
                }),
                audio_streams: vec![],
                container: "mp4".into(),
                start_timecode: None,
                file_size: None,
            };
            let file_clip = filmcraft_project::MediaClip {
                media: filmcraft_project::MediaRef::File { path: path.to_string_lossy().to_string() },
                info: file_info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            };
            let new_id = pr.add_item(&name, label, filmcraft_project::ItemKind::Media(Box::new(file_clip)), None);
            replacements.insert(orig_id, new_id);
        }

        let seq = pr.sequence_mut(nid).ok_or(EngineError::NoSequence)?;
        for t in seq.all_tracks_mut() {
            for it in &mut t.items {
                if let Some(new_id) = replacements.get(&it.item) {
                    it.item = *new_id;
                }
            }
        }
        Ok(())
    })?;

    Ok(())
}

/// Renders sequence `nid` to the linked master (`ProRes 4444` + alpha + PCM in QuickTime), so
/// FilmCraft has a real file to show before EffectCraft ever saves, and so the media watcher has
/// a baseline stamp to compare later renders with.
fn bake_linked_master(s: &Session, nid: ItemId, path: &Path, duration: Tick) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let settings = filmcraft_export::ExportSettings {
        format: filmcraft_export::Format::ProRes,
        path: path.to_string_lossy().into_owned(),
        prores_profile: "4444".into(),
        alpha: true,
        include_audio: true,
        range: Some(TimeRange::new(Tick::ZERO, duration)),
        ..Default::default()
    };
    let project = s.project.clone();
    let provider = s.media.full_res_provider(project.clone(), s.services.clone());
    filmcraft_export::export(&project, nid, &settings, &provider, &Default::default())
        .map_err(|e| EngineError::Other(format!("cannot render the CraftLink master: {e}")))?;
    Ok(())
}

/// Writes the sidecar manifest that names the composition, its timeline file and its master.
fn write_manifest(s: &Session, xml: &Path, master: &Path, name: &str, auto_sync: bool) -> Result<()> {
    let manifest = json!({
        "craftlink": MANIFEST_VERSION,
        "composition": name,
        "xml": xml.to_string_lossy(),
        "render": master.to_string_lossy(),
        "autoSync": auto_sync,
        "application": "FilmCraft",
    });
    let text = serde_json::to_vec_pretty(&manifest).map_err(|e| EngineError::Other(e.to_string()))?;
    let path = manifest_path(xml).to_string_lossy().to_string();
    s.services.write_file(&path, &text).map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(())
}

/// Points the clips placed for the nested composition at the linked master file instead, so the
/// timeline shows what EffectCraft renders. The nested composition itself stays in the project.
fn link_placed_clips_to_master(s: &mut Session, placed: &[ClipId], name: &str, master: &Path) -> Result<Option<ItemId>> {
    let path = master.to_string_lossy().to_string();
    let Ok(src) = s.media.open_file(&path, &*s.services) else {
        // Not readable yet: keep the nested composition on the timeline. The file becomes
        // readable as soon as EffectCraft writes it, and `craftlink.refresh` picks it up.
        return Ok(None);
    };
    let info = src.info().clone();
    let mut new_item = None;
    s.edit("Link CraftLink Master", |pr, _| {
        let clip = filmcraft_project::MediaClip {
            media: filmcraft_project::MediaRef::File { path: path.clone() },
            info: info.clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        };
        let id = pr.add_item(name, filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Media(Box::new(clip)), None);
        new_item = Some(id);
        let ids: Vec<ItemId> = pr.sequences().map(|s| s.id).collect();
        for sid in ids {
            let Some(seq) = pr.sequence_mut(sid) else { continue };
            for t in seq.all_tracks_mut() {
                for it in &mut t.items {
                    if placed.contains(&it.id) {
                        it.item = id;
                    }
                }
            }
        }
        Ok(())
    })?;
    if let Some(id) = new_item {
        s.media.insert_file(id, &path, src);
        // Baseline the stamp now: EffectCraft may overwrite the master before the first timed
        // media scan, and a first look must not treat the new file as "unchanged".
        s.note_media_stamp(&path);
    }
    Ok(new_item)
}

/// Core function to execute the CraftLink handoff to EffectCraft.
pub fn execute_bridge(s: &mut Session, p: &Value, replace_on_timeline: bool) -> Result<Value> {
    let name = commands::str_p(p, "name").filter(|n| !n.trim().is_empty()).map(str::to_string).unwrap_or_else(|| next_craftlink_comp_name(s));

    let (nid, placed_clips, span) = nest_selection_for_bridge(s, &name, replace_on_timeline)?;

    // If any items are procedural demo generators, bake them to real MP4 files so EffectCraft can show footage instead of a gray solid
    let _ = materialize_generators_for_export(s, nid);

    // Export the sequence to FCP7 XML
    let export_opts = filmcraft_interchange::ExportOptions { name: Some(name.clone()), relative_to: None, ..Default::default() };
    let (xml_bytes, _) =
        filmcraft_interchange::export(&s.project, nid, filmcraft_interchange::Format::Fcp7Xml, &export_opts).map_err(|e| EngineError::Other(e.to_string()))?;

    let out_path =
        if let Some(custom_path) = commands::str_p(p, "path") { PathBuf::from(custom_path) } else { link_folder(s).join(format!("{}.xml", file_stem(&name))) };

    // `Services::write_file` writes atomically and does not create folders (unlike
    // `write_file_new`), and the link folder does not exist until the first bridge run.
    // Without this, the very first "Send to EffectCraft" fails with a bare os error 3.
    if let Some(parent) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| EngineError::Other(format!("cannot create {}: {e}", parent.display())))?;
    }

    let path_str = out_path.to_string_lossy().to_string();
    s.services.write_file(&path_str, &xml_bytes).map_err(|e| EngineError::Other(e.to_string()))?;

    // CraftLink: a linked master FilmCraft shows, EffectCraft re-renders, and a manifest that
    // names both. A failure here never costs the handoff: the nested composition is already sent.
    let auto_sync = p.get("autoSync").and_then(Value::as_bool).unwrap_or(true);
    let master = master_path(s, &name);
    let mut link = json!({"linked": false, "render": master.to_string_lossy(), "autoSync": auto_sync});
    if replace_on_timeline {
        match bake_linked_master(s, nid, &master, span.duration) {
            Ok(()) => {
                if let Ok(Some(_)) = link_placed_clips_to_master(s, &placed_clips, &name, &master) {
                    link["linked"] = json!(true);
                } else {
                    link["error"] = json!("the master rendered but FilmCraft could not read it back");
                }
                if let Err(e) = write_manifest(s, &out_path, &master, &name, auto_sync) {
                    link["manifestError"] = json!(e.to_string());
                }
            }
            Err(e) => link["error"] = json!(e.to_string()),
        }
    } else if let Err(e) = write_manifest(s, &out_path, &master, &name, auto_sync) {
        link["manifestError"] = json!(e.to_string());
    }

    let effectcraft_result = if p.get("launch").and_then(Value::as_bool).unwrap_or(true) {
        launch_or_notify_effectcraft(&path_str)
    } else {
        json!({"status": "skipped", "message": "launch disabled by the caller"})
    };

    let verb = if replace_on_timeline { "Replaced and sent to EffectCraft" } else { "Sent to EffectCraft" };
    s.toast(format!("{verb}: {name}"));

    Ok(json!({
        "sequence": nid.0,
        "clips": placed_clips.iter().map(|c| c.0).collect::<Vec<_>>(),
        "path": path_str,
        "name": name,
        "start": span.start.0,
        "duration": span.duration.0,
        "effectcraft": effectcraft_result,
        "craftlink": link,
        "replaced": replace_on_timeline,
    }))
}

/// `craftlink.refresh`: read every media file that changed on disk again (the linked masters a
/// running EffectCraft wrote) without an undo step.
pub fn refresh_linked(s: &mut Session, _: &Value) -> Result<Value> {
    let r = crate::media_watch::refresh_changed(s, &Value::Null)?;
    let n = r["refreshed"].as_array().map(Vec::len).unwrap_or(0);
    s.toast(if n == 0 { "CraftLink: linked renders are up to date".to_string() } else { format!("CraftLink: updated {n} linked render(s)") });
    Ok(r)
}

/// Attempts to notify an existing EffectCraft instance, or launches a new one.
pub fn launch_or_notify_effectcraft(xml_path: &str) -> Value {
    if let Ok(()) = send_to_effectcraft_control(xml_path) {
        return json!({
            "status": "connected_to_control_server",
            "message": "Sent timeline to running EffectCraft instance"
        });
    }

    if let Some(exe) = find_effectcraft_exe() {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg(xml_path);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            const DETACHED_PROCESS: u32 = 0x00000008;
            cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        }
        match cmd.spawn() {
            Ok(child) => {
                return json!({
                    "status": "launched",
                    "pid": child.id(),
                    "executable": exe.to_string_lossy().to_string(),
                });
            }
            Err(e) => {
                return json!({
                    "status": "launch_failed",
                    "error": e.to_string(),
                    "executable": exe.to_string_lossy().to_string(),
                });
            }
        }
    }

    json!({
        "status": "saved_only",
        "message": "Exported XML timeline. Open EffectCraft and import this file.",
        "path": xml_path
    })
}

fn send_to_effectcraft_control(xml_path: &str) -> std::result::Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    let mut ports = vec![9877];
    // Missing or malformed discovery files just mean "try the default port".
    for file in [PathBuf::from("effectcraft_control.json"), std::env::temp_dir().join("effectcraft_control.json")] {
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
            "method": "engine.execute",
            "params": {
                "command": "file.importTimeline",
                "params": {
                    "path": xml_path
                }
            }
        });

        if writeln!(stream, "{}", req).is_err() {
            continue;
        }
        let mut reader = BufReader::new(stream);
        let mut resp = String::new();
        if reader.read_line(&mut resp).is_ok() && !resp.trim().is_empty() {
            return Ok(());
        }
    }
    Err("EffectCraft control channel unavailable".into())
}

/// Locates the EffectCraft executable across known locations.
pub fn find_effectcraft_exe() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("EFFECTCRAFT_PATH") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(curr) = std::env::current_exe()
        && let Some(parent) = curr.parent()
    {
        let candidates = [parent.join("effectcraft.exe"), parent.join("../EffectCraft/effectcraft.exe"), parent.join("../../EffectCraft/effectcraft.exe")];
        for c in candidates {
            if c.is_file() {
                return Some(c);
            }
        }
    }

    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let candidates = [Path::new(&program_files).join(r"ArtCraft\EffectCraft\effectcraft.exe"), Path::new(&program_files).join(r"EffectCraft\effectcraft.exe")];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let exe = dir.join("effectcraft.exe");
            if exe.is_file() {
                return Some(exe);
            }
            #[cfg(unix)]
            {
                let exe = dir.join("effectcraft");
                if exe.is_file() {
                    return Some(exe);
                }
            }
        }
    }

    None
}
