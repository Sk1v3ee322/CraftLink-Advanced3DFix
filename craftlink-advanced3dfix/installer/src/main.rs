//! CraftLink installer — puts CraftLink and the 3D layer styles into existing
//! FilmCraft / EffectCraft portable folders.
//!
//! This is the "no Rust" route: the finished programs are carried inside this
//! executable, so a person only has to name a folder. Nothing is downloaded,
//! nothing is compiled, and no source tree is needed.
//!
//! Safety rules it holds to:
//!   * `check` never writes; it only reports what would be replaced.
//!   * every folder is identified by the program it holds, and that program must be
//!     the exact official build this installer carries a replacement for — compared
//!     by SHA-256, not by name — so a different version is refused rather than
//!     overwritten.
//!   * the whole plan is decided before the first byte is written: one folder that
//!     does not fit stops the run with nothing changed anywhere.
//!   * the old program is copied to `_craftlink-backup/<date>/` first.
//!   * nothing else in the folder is touched: `data/`, `portable.txt`, licences and
//!     the command-line program stay exactly as they were.
//!   * `uninstall` puts the previous program back.

mod sha256;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The programs this installer can put in place, compiled into the binary.
///
/// `include_bytes!` paths are relative to this source file (`installer/src/`).
const PAYLOADS: &[Payload] = &[
    Payload {
        product: "FilmCraft",
        exe: "filmcraft.exe",
        dir_hint: "filmcraft-0.20.1-windows-x64-portable",
        bytes: include_bytes!("../payload/filmcraft.exe"),
        version: "0.20.1",
        release_sha256: "2bd02ddc541766b736dc238651bd3c018466c52da64cbabf454a474db7f51663",
    },
    Payload {
        product: "EffectCraft",
        exe: "effectcraft.exe",
        dir_hint: "effectcraft-0.24.1-windows-x64-portable",
        bytes: include_bytes!("../payload/effectcraft.exe"),
        version: "0.24.1",
        release_sha256: "91654a6e1d28011c4b4972908640dad106d29b92d11f2a1a1cefabd1593e1355",
    },
];

struct Payload {
    /// How the product is named in the official release. Also what a person calls it.
    product: &'static str,
    exe: &'static str,
    /// The folder name the official archive unpacks to; used only to look one level
    /// deeper when a person names the folder that holds the unpacked folder.
    dir_hint: &'static str,
    bytes: &'static [u8],
    version: &'static str,
    /// SHA-256 of the official portable program this is a drop-in replacement for.
    release_sha256: &'static str,
}

/// What a folder holds now.
enum State {
    /// The official release this installer was built to replace.
    Release,
    /// Already carrying exactly the build in this installer.
    Installed,
    /// Some other build — a different version, or one already changed.
    Unknown,
}

/// One portable folder this run will act on.
struct Found {
    dir: PathBuf,
    payload: &'static Payload,
}

/// Folder holding the previous program, inside the portable folder.
const BACKUP_DIR: &str = "_craftlink-backup";
/// Marker written next to the backup, naming what this run replaced.
const REPLACED_LIST: &str = "REPLACED.txt";

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        // An empty message means the reason was already printed in both languages.
        Err(message) if message.is_empty() => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => interactive(None),
        Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some("list") => {
            list();
            Ok(())
        }
        Some("check") => {
            let path = args
                .get(1)
                .ok_or("`check` needs a folder: check <folder>")?;
            check(Path::new(path))
        }
        Some("apply") | Some("install") => {
            let path = args
                .get(1)
                .ok_or("`install` needs a folder: install <folder>")?;
            install(Path::new(path), true)
        }
        Some("uninstall") | Some("revert") => {
            let path = args
                .get(1)
                .ok_or("`uninstall` needs a folder: uninstall <folder>")?;
            uninstall(Path::new(path))
        }
        // Dragging a folder onto the executable: Windows passes it as the argument.
        Some(other) if Path::new(other).is_dir() => interactive(Some(Path::new(other))),
        Some(other) => Err(format!("unknown command `{other}`; try `help`")),
    }
}

fn print_help() {
    println!(
        "\
CraftLink + 3D layer styles — installer

WHAT THIS DOES
  Puts two features into FilmCraft 0.20.1 and EffectCraft 0.24.1:
    * CraftLink: send clips from FilmCraft to EffectCraft and see the result come back
      on the FilmCraft timeline by itself.
    * Layer styles on 3D text: Stroke, Inner Shadow, Drop Shadow and the rest now follow
      the silhouette of extruded 3D text and shapes.

  The ready programs are inside this file, so nothing is downloaded and nothing is
  compiled: no Rust, no git, no source folder. You only name a folder.

WHAT YOU NEED
  The portable folder of either release — the one that holds filmcraft.exe or
  effectcraft.exe together with portable.txt.

  You may also name the folder that CONTAINS the unpacked portable folders, for
  example your Downloads folder: both products are then found and handled in one run.

EASIEST WAY — run it with no arguments and answer the question
  CraftLink-Installer

USAGE
  CraftLink-Installer                     interactive: find, show, confirm
  CraftLink-Installer check <folder>      dry run; reports what would be replaced
  CraftLink-Installer install <folder>    replace the program (a backup is made first)
  CraftLink-Installer uninstall <folder>  put the previous program back
  CraftLink-Installer list                show what is carried inside
  CraftLink-Installer help                this text

SAFETY
  * `check` writes nothing at all.
  * the folder must hold the official program this installer was built for, matched by
    its SHA-256; anything else is refused before a byte is written.
  * the whole plan is checked before anything is written: if one folder does not fit,
    nothing changes anywhere.
  * the previous program is copied to _craftlink-backup/<date>/ inside its portable
    folder, so `uninstall` is a real undo.
  * no other file is touched — data/, portable.txt, the licences and the command-line
    program stay exactly as they were.

AFTER INSTALLING
  Just start the applications as usual. The new features are in the File menu (EffectCraft
  handoff) and in Layer Styles (3D text and shapes)."
    );
}

fn list() {
    println!("Carried inside this installer:");
    for p in PAYLOADS {
        println!(
            "  {:11} {}  version {}  {:.1} MB",
            p.product,
            p.exe,
            p.version,
            p.bytes.len() as f64 / 1_048_576.0
        );
    }
}

/// The program this folder holds, if it holds one this installer knows.
fn detect(dir: &Path) -> Option<&'static Payload> {
    PAYLOADS.iter().find(|p| dir.join(p.exe).is_file())
}

/// Every portable folder at or below `path` that this installer can act on.
///
/// A person may name the portable folder itself, the folder above it (as an archive
/// usually unpacks to `name/name/`), or a folder holding several products.
fn resolve(path: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    collect(path, 0, &mut found);
    found
}

fn collect(dir: &Path, depth: usize, found: &mut Vec<Found>) {
    if let Some(payload) = detect(dir) {
        found.push(Found {
            dir: dir.to_path_buf(),
            payload,
        });
        // A portable folder holds one program; there is nothing to look for inside it.
        return;
    }
    if depth >= 2 {
        return;
    }
    let mut children: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return,
    };
    children.sort();
    for child in children {
        if child.is_dir() {
            collect(&child, depth + 1, found);
        }
    }
}

fn nothing_found(path: &Path) -> String {
    let names: Vec<&str> = PAYLOADS.iter().map(|p| p.exe).collect();
    let hints: Vec<&str> = PAYLOADS.iter().map(|p| p.dir_hint).collect();
    format!(
        "Nothing to install into under {}.\n\n\
         Looked for {}, in that folder, one level below it, and two levels below it.\n\n\
         Name a portable folder of the release — one that holds {} together with\n\
         portable.txt, for example:\n  {}\n\
         or the folder that contains the unpacked folders.\n\n\
         A source folder (the one with apps/…/Cargo.toml) is not what this installer\n\
         wants: it replaces the finished program, it does not compile one.\n\n\
         Nothing was written.",
        show(path),
        names.join(" or "),
        names.join(" / "),
        hints.join("\n  ")
    )
}

fn check(path: &Path) -> Result<()> {
    let path = joinable(path)?;
    let found = resolve(&path);
    if found.is_empty() {
        return Err(nothing_found(&path));
    }
    println!("Under: {}\n", show(&path));
    for f in &found {
        let target = f.dir.join(f.payload.exe);
        let current =
            fs::read(&target).map_err(|e| format!("cannot read {}: {e}", target.display()))?;
        let state = classify(&current, f.payload);
        println!("{} {}  ({})", f.payload.product, f.payload.version, show(&f.dir));
        println!("  now           : {} bytes", current.len());
        println!("  this installer: {} bytes", f.payload.bytes.len());
        match state {
            State::Release => {
                println!("  plan          : replace with the CraftLink / 3D-layer-style build")
            }
            State::Installed => println!("  plan          : nothing — this build is already there"),
            State::Unknown => {
                println!("  plan          : REFUSED");
                println!();
                println!("{}", unknown_advice(f.payload, current.len() as u64));
                println!();
                continue;
            }
        }
        println!();
    }
    if found
        .iter()
        .any(|f| matches!(state_of(f), State::Unknown))
    {
        println!("At least one folder does not fit this installer; `install` would refuse it.");
    } else {
        println!("Nothing was written. Run `install` to put the new programs in place.");
    }
    Ok(())
}

/// The state of a folder that was already validated to exist.
fn state_of(f: &Found) -> State {
    match fs::read(f.dir.join(f.payload.exe)) {
        Ok(current) => classify(&current, f.payload),
        // A folder that cannot be read is certainly not the release.
        Err(_) => State::Unknown,
    }
}

/// Which build the folder holds, decided by content rather than by file name.
fn classify(current: &[u8], payload: &Payload) -> State {
    if current == payload.bytes {
        State::Installed
    } else if sha256_hex(current) == payload.release_sha256 {
        State::Release
    } else {
        State::Unknown
    }
}

fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in sha256::digest(data) {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn unknown_advice(payload: &Payload, size: u64) -> String {
    format!(
        "The folder holds a {} program this installer does not replace.\n\n\
         Found : {}, {} bytes\n\
         Wanted: the official portable {} {} — recognised by its SHA-256, not by its name.\n\n\
         That happens when the folder holds a different version, or a build that has already\n\
         been changed. Install into a freshly unpacked portable folder of the release.\n\n\
         Nothing was written.",
        payload.product, payload.exe, size, payload.product, payload.version
    )
}

/// Decide everything first, then write. One folder that does not fit stops the run.
fn install(path: &Path, ask: bool) -> Result<()> {
    let path = joinable(path)?;
    let found = resolve(&path);
    if found.is_empty() {
        return Err(nothing_found(&path));
    }

    // --- the whole plan, decided before anything is touched
    let mut work: Vec<(&Found, Vec<u8>)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut already = 0usize;
    for f in &found {
        let target = f.dir.join(f.payload.exe);
        let current = match fs::read(&target) {
            Ok(bytes) => bytes,
            Err(e) => {
                refused.push(format!("{}: cannot read {}: {e}", f.payload.product, show(&target)));
                continue;
            }
        };
        match classify(&current, f.payload) {
            State::Release => work.push((f, current)),
            State::Installed => already += 1,
            State::Unknown => refused.push(format!(
                "{}: {} ({} bytes) is not the official {} {}",
                f.payload.product,
                show(&target),
                current.len(),
                f.payload.product,
                f.payload.version
            )),
        }
    }

    if !refused.is_empty() {
        for line in &refused {
            eprintln!("refused: {line}");
        }
        return Err(format!(
            "{} folder(s) do not fit this installer.\n\
             Nothing was written anywhere — not even into the folders that did fit.",
            refused.len()
        ));
    }
    if work.is_empty() {
        println!(
            "Nothing to do: {} folder(s) already carry this build.",
            already
        );
        return Ok(());
    }

    println!("Under: {}\n", show(&path));
    for (f, current) in &work {
        println!("{} {}  ({})", f.payload.product, f.payload.version, show(&f.dir));
        println!(
            "  {} → {} bytes",
            current.len(),
            f.payload.bytes.len()
        );
        println!("  previous copy → {}/{}/", BACKUP_DIR, timestamp());
        println!();
    }
    if already > 0 {
        println!("{} folder(s) already have this build and will be left alone.\n", already);
    }

    if ask && !confirm("Install now? [y/N] ")? {
        println!("Cancelled; nothing was written.");
        return Ok(());
    }

    let stamp = timestamp();
    for (f, current) in &work {
        let target = f.dir.join(f.payload.exe);
        let backup_root = f.dir.join(BACKUP_DIR).join(&stamp);
        fs::create_dir_all(&backup_root)
            .map_err(|e| format!("cannot create {}: {e}", show(&backup_root)))?;
        let backup = backup_root.join(f.payload.exe);
        fs::write(&backup, current)
            .map_err(|e| format!("cannot back up {}: {e}", show(&backup)))?;
        let mut list = String::from(
            "# What this run replaced, relative to the portable folder. `uninstall` restores it.\n",
        );
        list.push_str(f.payload.exe);
        list.push('\n');
        list.push_str(&format!("{} {}\n", f.payload.product, f.payload.version));
        let list_path = backup_root.join(REPLACED_LIST);
        fs::write(&list_path, list.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", show(&list_path)))?;

        fs::write(&target, f.payload.bytes)
            .map_err(|e| format!("cannot write {}: {e}", show(&target)))?;

        // Prove it: read back and compare.
        let written =
            fs::read(&target).map_err(|e| format!("cannot re-read {}: {e}", show(&target)))?;
        if written != f.payload.bytes {
            return Err(format!(
                "{} does not match what was written; the previous copy is in {}",
                show(&target),
                show(&backup_root)
            ));
        }
        println!(
            "Installed {} {} into {}",
            f.payload.product,
            f.payload.version,
            show(&f.dir)
        );
    }

    println!("\nStart the applications as usual — the new features are already in them.");
    Ok(())
}

fn uninstall(path: &Path) -> Result<()> {
    let path = joinable(path)?;
    let found = resolve(&path);
    if found.is_empty() {
        return Err(nothing_found(&path));
    }

    let mut restored = 0usize;
    let mut skipped = 0usize;
    for f in &found {
        let root = f.dir.join(BACKUP_DIR);
        let newest = match newest_backup(&root) {
            Some(dir) => dir,
            None => {
                skipped += 1;
                continue;
            }
        };
        let backup = newest.join(f.payload.exe);
        if !backup.is_file() {
            skipped += 1;
            continue;
        }
        let target = f.dir.join(f.payload.exe);
        let previous =
            fs::read(&backup).map_err(|e| format!("cannot read {}: {e}", show(&backup)))?;
        fs::write(&target, &previous)
            .map_err(|e| format!("cannot restore {}: {e}", show(&target)))?;
        println!(
            "Restored {} {} ({} bytes) in {}",
            f.payload.product,
            f.payload.version,
            previous.len(),
            show(&f.dir)
        );
        restored += 1;
    }

    if restored == 0 {
        return Err(format!(
            "no {} folder holds a backup under {}; nothing was restored.\n\
             This folder was not changed by this installer, or the backup was deleted.",
            BACKUP_DIR,
            show(&path)
        ));
    }
    if skipped > 0 {
        println!(
            "\n{} folder(s) had no backup and were left as they are.",
            skipped
        );
    }
    println!("\nThe portable folder(s) are back to what they were before the installer ran.");
    Ok(())
}

/// The newest backup folder under `root`, by name (the names sort by date).
fn newest_backup(root: &Path) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    names.sort();
    names.pop()
}

fn interactive(start: Option<&Path>) -> Result<()> {
    println!("CraftLink + 3D layer styles — installer");
    println!("======================================\n");
    list();
    println!();
    println!("The ready programs are inside this file: nothing is downloaded, nothing is");
    println!("compiled, and no Rust or source folder is needed.\n");
    println!("Готовые программы уже внутри этого файла: ничего не скачивается и не собирается,");
    println!("Rust и папка с исходниками не нужны.\n");

    let path = match start {
        Some(path) => joinable(path)?,
        None => {
            let answer = prompt(
                "Folder to install into — your portable folder, or the folder that\n\
                 contains the unpacked portable folders\n\
                 Папка: portable-папка (где лежит filmcraft.exe / effectcraft.exe)\n\
                 или папка, в которой лежат распакованные portable-папки: ",
            )?;
            if answer.is_empty() {
                println!("Nothing entered; exiting.");
                return Ok(());
            }
            joinable(Path::new(&answer))?
        }
    };

    let found = resolve(&path);
    if found.is_empty() {
        return Err(nothing_found(&path));
    }
    println!("\nFound under {}:", show(&path));
    for f in &found {
        let state = match state_of(f) {
            State::Release => "official release — can be installed",
            State::Installed => "already has this build — can be uninstalled",
            State::Unknown => "NOT the official build — will be refused",
        };
        println!("  {} {} in {}\n      {}", f.payload.product, f.payload.version, show(&f.dir), state);
    }
    println!();

    // Nothing installable but something already installed: offer the undo.
    let installable = found.iter().any(|f| matches!(state_of(f), State::Release));
    if !installable {
        let undoable = found.iter().any(|f| matches!(state_of(f), State::Installed));
        if !undoable {
            return Err(format!(
                "none of the folders under {} is the release this installer was built for,\n\
                 so nothing can be done.\n\n\
                 Ни одна папка не подходит: нужна официальная сборка.\n\n\
                 Nothing was written.",
                show(&path)
            ));
        }
        println!("This already has the build. / Здесь уже эта сборка.\n");
        println!("  1  uninstall — put the previous program back / вернуть как было");
        println!("  2  quit      — выход");
        if prompt("\nChoose [1/2]: ")?.trim() == "1" {
            println!();
            return uninstall(&path);
        }
        println!("Nothing was written. / Ничего не записано.");
        return Ok(());
    }

    println!("  1  install — replace the program(s), a backup is made first");
    println!("             заменить (сначала бэкап)");
    println!("  2  quit    — выход");
    if prompt("\nChoose [1/2]: ")?.trim() != "1" {
        println!("Nothing was written. / Ничего не записано.");
        return Ok(());
    }
    println!();
    install(&path, false)
}

fn joinable(dir: &Path) -> Result<PathBuf> {
    let path =
        fs::canonicalize(dir).map_err(|e| format!("cannot open `{}`: {e}", dir.display()))?;
    if !path.is_dir() {
        return Err(format!("`{}` is not a folder", show(&path)));
    }
    Ok(path)
}

/// A path as a person wants to read it: Windows' `\\?\` prefix only looks broken.
fn show(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("cannot read input: {e}"))?;
    Ok(trim_input(&line).to_string())
}

fn confirm(label: &str) -> Result<bool> {
    let answer = prompt(label)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "д" | "да"
    ))
}

/// Dragging a folder into a console wraps it in quotes and may leave a trailing space.
fn trim_input(raw: &str) -> &str {
    let mut text = raw.trim();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text = &text[1..text.len() - 1];
    }
    text.trim()
}

/// A readable, sortable folder name, e.g. `2026-11-17_153045`.
fn timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("sandbox");
        dir
    }

    #[test]
    fn every_payload_is_carried_and_plausible() {
        assert_eq!(PAYLOADS.len(), 2, "one program per product");
        for p in PAYLOADS {
            assert!(
                p.bytes.len() > 10_000_000,
                "{} looks truncated ({} bytes)",
                p.exe,
                p.bytes.len()
            );
            // A Windows executable, not an empty placeholder.
            assert_eq!(&p.bytes[..2], b"MZ", "{} is not a PE file", p.exe);
        }
    }

    #[test]
    fn the_two_payloads_are_different_programs() {
        assert_ne!(
            PAYLOADS[0].bytes.len(),
            PAYLOADS[1].bytes.len(),
            "the two products differ in size"
        );
    }

    #[test]
    fn the_release_hash_is_a_full_sha256() {
        for p in PAYLOADS {
            assert_eq!(p.release_sha256.len(), 64, "{}: a full digest", p.exe);
            assert!(
                p.release_sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{}: lowercase hex only",
                p.exe
            );
            // A digest equal to the carried build would mean the wrong value was pasted
            // in: this field names the *official* release, which the carried build is not.
            assert_ne!(
                sha256_hex(p.bytes),
                p.release_sha256,
                "{}: the carried build must differ from the release it replaces",
                p.exe
            );
        }
    }

    #[test]
    fn a_foreign_build_is_not_mistaken_for_the_release() {
        let payload = &PAYLOADS[0];
        // Neither the release nor the carried build: a future version, or a changed one.
        let foreign = vec![b'X'; 4096];
        assert!(
            matches!(classify(&foreign, payload), State::Unknown),
            "an unrelated file must be refused, never overwritten"
        );
        // The carried build is recognised as already installed, so `install` is a no-op.
        assert!(
            matches!(classify(payload.bytes, payload), State::Installed),
            "the carried build is recognised"
        );
    }

    #[test]
    fn a_foreign_build_is_refused_with_an_explanation() {
        let text = unknown_advice(&PAYLOADS[0], 4096);
        assert!(text.contains("does not replace"), "says why: {text}");
        assert!(text.contains("Nothing was written"), "promises no writes: {text}");
        assert!(text.contains("4096"), "reports what it found: {text}");
    }

    #[test]
    fn the_sha256_helper_matches_published_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "the empty-string vector"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "the FIPS 180-4 'abc' vector"
        );
    }

    #[test]
    fn a_portable_folder_is_found_when_named_directly() {
        let root = sandbox("craftlink-resolve-direct");
        fs::write(root.join("filmcraft.exe"), b"MZ").expect("exe");
        let found = resolve(&root);
        assert_eq!(found.len(), 1, "one folder found");
        assert_eq!(found[0].payload.exe, "filmcraft.exe");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_archive_style_nesting_is_looked_through() {
        // An archive unpacks to `name/name/`, so a person may name either level.
        let root = sandbox("craftlink-resolve-nested");
        let inner = root.join("filmcraft-0.20.1-windows-x64-portable");
        fs::create_dir_all(&inner).expect("inner");
        fs::write(inner.join("filmcraft.exe"), b"MZ").expect("exe");
        let found = resolve(&root);
        assert_eq!(found.len(), 1, "the nested folder is found");
        assert_eq!(found[0].dir, inner);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn both_products_are_found_in_one_run() {
        let root = sandbox("craftlink-resolve-both");
        for exe in ["filmcraft.exe", "effectcraft.exe"] {
            let d = root.join(exe.trim_end_matches(".exe"));
            fs::create_dir_all(&d).expect("product folder");
            fs::write(d.join(exe), b"MZ").expect("exe");
        }
        let found = resolve(&root);
        assert_eq!(found.len(), 2, "both products found");
        let names: Vec<&str> = found.iter().map(|f| f.payload.exe).collect();
        assert!(names.contains(&"filmcraft.exe") && names.contains(&"effectcraft.exe"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_folder_finds_nothing() {
        let root = sandbox("craftlink-resolve-empty");
        assert!(resolve(&root).is_empty(), "nothing to act on");
        let text = nothing_found(&root);
        assert!(text.contains("filmcraft.exe"), "names what it looked for: {text}");
        assert!(text.contains("Nothing was written"), "promises no writes: {text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_date_is_a_real_calendar_date() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_322), (2025, 8, 22));
    }

    #[test]
    fn dragging_a_folder_strips_quotes_and_space() {
        assert_eq!(trim_input("\"C:\\Some Folder\\App\" "), "C:\\Some Folder\\App");
        assert_eq!(trim_input("  C:\\App  "), "C:\\App");
        assert_eq!(trim_input("\"C:\\App\""), "C:\\App");
    }

    #[test]
    fn a_backup_folder_sorts_by_name() {
        let root = sandbox("craftlink-backup-test");
        fs::create_dir_all(root.join("2026-11-17_100000")).expect("older");
        fs::create_dir_all(root.join("2026-11-18_090000")).expect("newer");
        let newest = newest_backup(&root).expect("a backup is found");
        assert!(
            newest.ends_with("2026-11-18_090000"),
            "the newest backup is picked: {}",
            newest.display()
        );
        let _ = fs::remove_dir_all(&root);
    }
}