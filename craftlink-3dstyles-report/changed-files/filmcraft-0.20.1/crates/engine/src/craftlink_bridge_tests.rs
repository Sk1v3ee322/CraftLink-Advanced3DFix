//! Unit tests for the CraftLink bridge (FilmCraft ↔ EffectCraft).

use serde_json::json;

use filmcraft_project::TrackKind;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn clip_ids(s: &Session, kind: TrackKind, idx: usize) -> Vec<u64> {
    s.active_sequence().unwrap().tracks(kind)[idx].items.iter().map(|i| i.id.0).collect()
}

#[test]
fn craftlink_fails_when_no_clips_selected() {
    let mut s = demo();
    s.state.selection.clear();
    let err = s.execute("clip.replaceWithCraftLink", json!({})).unwrap_err();
    assert!(err.to_string().contains("no clips selected"));
}

#[test]
fn replace_with_craftlink_creates_xml_and_replaces_on_timeline() {
    let mut s = demo();
    let seq_id = s.state.active_sequence.unwrap();
    let v_clips = clip_ids(&s, TrackKind::Video, 0);
    assert!(v_clips.len() >= 2);
    let (c1, c2) = (v_clips[0], v_clips[1]);
    s.execute("timeline.select", json!({"clips": [c1, c2]})).unwrap();

    let dir = std::env::temp_dir().join(format!("craftlink_test_replace_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let tmp_path = dir.join("MyCraftComp.xml").to_string_lossy().to_string();

    let r = s
        .execute(
            "clip.replaceWithCraftLink",
            json!({
                "name": "MyCraftComp",
                "path": tmp_path, "launch": false,
            }),
        )
        .unwrap();

    assert_eq!(r["name"], "MyCraftComp");
    assert_eq!(r["replaced"], true);
    assert_eq!(r["path"], tmp_path);

    // Verify XML exists and is valid FCP7 XML
    let content = std::fs::read_to_string(&tmp_path).unwrap();
    assert!(content.contains("<xmeml"));

    // The sidecar manifest names the composition and its linked master.
    let manifest = dir.join("MyCraftComp.craftlink.json");
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"craftlink\""));
    assert!(text.contains("MyCraftComp"));

    // Verify timeline on active sequence: original c1 and c2 are replaced
    let seq = s.project.sequence(seq_id).unwrap();
    assert!(!seq.video_tracks[0].items.iter().any(|i| i.id.0 == c1 || i.id.0 == c2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn send_to_craftlink_keeps_original_timeline() {
    let mut s = demo();
    let seq_id = s.state.active_sequence.unwrap();
    let v_clips = clip_ids(&s, TrackKind::Video, 0);
    assert!(v_clips.len() >= 2);
    let (c1, c2) = (v_clips[0], v_clips[1]);
    s.execute("timeline.select", json!({"clips": [c1, c2]})).unwrap();

    let dir = std::env::temp_dir().join(format!("craftlink_test_send_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let tmp_path = dir.join("SentComp.xml").to_string_lossy().to_string();

    let r = s
        .execute(
            "clip.sendToCraftLink",
            json!({
                "name": "SentComp",
                "path": tmp_path, "launch": false,
            }),
        )
        .unwrap();

    assert_eq!(r["name"], "SentComp");
    assert_eq!(r["replaced"], false);

    // Verify XML exists
    let content = std::fs::read_to_string(&tmp_path).unwrap();
    assert!(content.contains("<xmeml"));

    // Verify original clips remain on timeline
    let seq = s.project.sequence(seq_id).unwrap();
    assert!(seq.video_tracks[0].items.iter().any(|i| i.id.0 == c1));
    assert!(seq.video_tracks[0].items.iter().any(|i| i.id.0 == c2));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A fade-out at the end of the last selected clip survives as a one-sided (black) transition
/// instead of being dropped with the unselected neighbour.
#[test]
fn boundary_transition_becomes_a_one_sided_fade() {
    let mut s = demo();
    let v_clips = clip_ids(&s, TrackKind::Video, 0);
    assert!(v_clips.len() >= 2);
    // The demo has a cross dissolve between clips 0 and 1; send clip 0 alone.
    s.execute("timeline.select", json!({"clips": [v_clips[0]]})).unwrap();

    let dir = std::env::temp_dir().join(format!("craftlink_test_fade_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let tmp_path = dir.join("FadeComp.xml").to_string_lossy().to_string();
    let r = s.execute("clip.sendToCraftLink", json!({"name": "FadeComp", "path": tmp_path, "launch": false})).unwrap();
    assert_eq!(r["replaced"], false);

    let content = std::fs::read_to_string(&tmp_path).unwrap();
    assert!(content.contains("end-black"), "the trailing fade-out must survive: {content}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A cross dissolve whose two sides are both selected is copied whole: the boundary trimming is
/// only for a transition that lost its other side to black.
#[test]
fn a_cross_dissolve_between_two_selected_clips_keeps_both_sides() {
    let mut s = demo();
    let seq_id = s.state.active_sequence.unwrap();
    let orig = s.project.sequence(seq_id).unwrap().video_tracks[0]
        .transitions
        .iter()
        .find(|t| t.from.is_some() && t.to.is_some())
        .cloned()
        .expect("the demo has a cross dissolve between its first two clips");
    let (from, to) = (orig.from.unwrap(), orig.to.unwrap());
    s.execute("timeline.select", json!({"clips": [from.0, to.0]})).unwrap();

    let (nid, _, _) = crate::craftlink_bridge::nest_selection_for_bridge(&mut s, "DissolveComp", false).unwrap();
    let nested = s.project.sequence(nid).unwrap();
    let copied = nested.video_tracks[0].transitions.iter().find(|t| t.from.is_some() && t.to.is_some()).expect("the cross dissolve must keep both sides");
    assert_eq!(copied.duration, orig.duration, "a fully selected dissolve is not trimmed");
}

/// The very first bridge run in a project has no `CraftLink` folder yet, and `Services::write_file`
/// is atomic without creating folders. Before the fix that first "Send to EffectCraft" failed with
/// a bare `os error 3` instead of writing the XML and the manifest.
#[test]
fn the_first_bridge_run_creates_its_link_folder() {
    let mut s = demo();
    s.path = Some("/nowhere/never-saved/Project.fcproj".to_string());

    // The folder the default path lives in must not exist before the run.
    let folder = crate::craftlink_bridge::link_folder(&s);
    let _ = std::fs::remove_dir_all(&folder);
    assert!(!folder.exists(), "precondition: the link folder is absent");

    let v_clips = clip_ids(&s, TrackKind::Video, 0);
    s.execute("timeline.select", json!({"clips": [v_clips[0]]})).unwrap();

    let r = s.execute("clip.sendToCraftLink", json!({"name": "FirstComp", "launch": false})).unwrap();

    let xml = std::path::PathBuf::from(r["path"].as_str().expect("path"));
    assert!(xml.exists(), "the XML must be written into a folder created on demand");
    assert!(folder.exists(), "the link folder must exist after the first run");
    let manifest = crate::craftlink_bridge::manifest_path(&xml);
    assert!(manifest.exists(), "the sidecar manifest must be written next to the XML");

    let _ = std::fs::remove_dir_all(&folder);
}
