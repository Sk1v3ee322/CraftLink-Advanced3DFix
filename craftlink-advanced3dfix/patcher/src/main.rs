//! CraftLink patcher — applies the CraftLink / 3D layer-style patches to a release tree.
//!
//! Built for recipients who do not have `git` or `patch` installed and would rather
//! not edit files by hand. The patches are embedded in this binary, so the executable
//! is the whole tool: no network, no dependencies, no sibling files.
//!
//! Safety rules it holds to:
//!   * `check` never writes; it is the default first step and safe to repeat.
//!   * `apply` first runs the whole check, and refuses to touch anything if one
//!     context line disagrees — so it cannot half-patch a different release.
//!   * every file it overwrites is copied to `_craftlink-backup/<stamp>/` first, and
//!     the names of the files it creates are recorded there too, so `uninstall` can
//!     put the tree back exactly as it was, including removing what was added.
//!   * after writing, each file is re-hashed and compared with `FILES.txt`, the same
//!     manifest the report ships.

mod patch;
mod sha256;

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The patches, embedded at build time. Rebuilding after the patches change is the
/// only way this binary can fall out of date.
const PATCHES: [(&str, &str, &str, &str); 3] = [
    (
        "01",
        "filmcraft-0.20.1-craftlink.patch",
        "filmcraft",
        include_str!("../../01-filmcraft-0.20.1-craftlink.patch"),
    ),
    (
        "02",
        "effectcraft-0.24.1-craftlink.patch",
        "effectcraft",
        include_str!("../../02-effectcraft-0.24.1-craftlink.patch"),
    ),
    (
        "03",
        "effectcraft-0.24.1-3d-layer-styles.patch",
        "effectcraft",
        include_str!("../../03-effectcraft-0.24.1-3d-layer-styles.patch"),
    ),
];

const MANIFEST: &str = include_str!("../../FILES.txt");
const BACKUP_DIR: &str = "_craftlink-backup";

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    };
    code
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
            let dir = args.get(1).ok_or("`check` needs a folder: check <release-folder>")?;
            check(Path::new(dir))
        }
        Some("apply") => {
            let dir = args.get(1).ok_or("`apply` needs a folder: apply <release-folder>")?;
            apply(Path::new(dir), true)
        }
        Some("revert") => {
            let dir = args.get(1).ok_or("`revert` needs a folder: revert <release-folder>")?;
            revert(Path::new(dir), false)
        }
        Some("uninstall") => {
            let dir = args
                .get(1)
                .ok_or("`uninstall` needs a folder: uninstall <release-folder>")?;
            revert(Path::new(dir), true)
        }
        // Dragging a folder onto the executable is how most people will start it:
        // Windows passes the folder as the only argument. Treat that as the start of
        // the interactive flow instead of "unknown command".
        Some(other) if Path::new(other).is_dir() => interactive(Some(Path::new(other))),
        Some(other) => Err(format!("unknown command `{other}`; try `help`")),
    }
}

fn print_help() {
    println!(
        "\
CraftLink + 3D layer styles — patcher

WHAT THIS DOES
  Adds two features to FilmCraft 0.20.1 and EffectCraft 0.24.1:
    * CraftLink: send clips from FilmCraft to EffectCraft and see the result come back
      on the FilmCraft timeline by itself.
    * Layer styles on 3D text: Stroke, Inner Shadow, Drop Shadow and the rest now follow
      the silhouette of extruded 3D text and shapes.
  It edits a copy of the application's source folder. It does not install anything and
  does not touch the applications you already have installed.

WHAT YOU NEED
  The source folder of the release, as unpacked from the official archive:
    FilmCraft    the folder that contains apps/filmcraft/Cargo.toml
    EffectCraft  the folder that contains apps/effectcraft/Cargo.toml
  A Rust toolchain (the `cargo` command) to build the result afterwards.

  NOT an installed or portable copy. Those hold filmcraft.exe / effectcraft.exe, which are
  already compiled, while these patches change source code and the application must be
  rebuilt. To put the features into a portable build: patch the sources, run
  `cargo build --release`, then copy target/release/<product>.exe into the portable folder
  over the old file (keep a copy of it first).

EASIEST WAY — just run it with no arguments and answer the questions
  craftlink-patcher

USAGE
  craftlink-patcher                     interactive menu: check, apply, uninstall
  craftlink-patcher check <folder>      dry run: report what would change, write nothing
  craftlink-patcher apply <folder>      run the check, then patch (asks before writing)
  craftlink-patcher uninstall <folder>  put the folder back exactly as it was
  craftlink-patcher revert <folder>     restore only the overwritten files
  craftlink-patcher list                show the embedded patches
  craftlink-patcher help                this text

  <folder> is the release folder itself. Name it on the command line with quotes if it
  contains spaces, or drag the folder into the window.

WHAT IT CHANGES
  FilmCraft   27 files: 23 replaced, 4 new (two of them are the .md documents)
  EffectCraft 22 files: 19 replaced, 3 new (two of them are the .md documents)

SAFETY
  * `check` writes nothing at all.
  * `apply` proves every line first. If the folder is not the release this was built
    for, it stops before writing anything and says which line disagreed.
  * Anything it overwrites is copied to _craftlink-backup/<date>/ inside the release
    folder, and the files it creates are listed there, so `uninstall` is a real undo.

AFTER PATCHING
  cargo build --release
  cargo test -p filmcraft-engine craftlink      (FilmCraft:  6 tests)
  cargo test -p effectcraft-engine craftlink    (EffectCraft: 10 tests)

AFTERWARDS
  `craftlink-patcher uninstall <folder>` puts the source folder back the way it was.
  Then rebuild with `cargo build --release`.

This program embeds the three patches and needs no network, no git and no other files."
    );
}

fn list() {
    println!("Embedded patches:");
    for (id, name, product, text) in PATCHES {
        let sections = patch::parse(text).map(|s| s.len()).unwrap_or(0);
        println!(
            "  {id}  {name:<44} {product:<12} {sections} files, {} bytes",
            text.len()
        );
    }
    println!(
        "\nFILES.txt: {} files with a recordable sha256 prefix",
        manifest_rows().len()
    );
}

/// One row of the shipped `FILES.txt`.
struct Row {
    product: String,
    path: String,
    bytes: usize,
    digest: String,
}

fn manifest_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for line in MANIFEST.lines().skip(1) {
        let mut parts = line.split('\t');
        let (Some(product), Some(path), Some(bytes), Some(digest)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(bytes) = bytes.parse::<usize>() else {
            continue;
        };
        rows.push(Row {
            product: product.to_string(),
            path: path.to_string(),
            bytes,
            digest: digest.to_string(),
        });
    }
    rows
}

/// Which release this folder is, and therefore which patches apply.
fn detect(dir: &Path) -> Option<&'static str> {
    if dir.join("apps").join("filmcraft").join("Cargo.toml").is_file() {
        return Some("filmcraft");
    }
    if dir
        .join("apps")
        .join("effectcraft")
        .join("Cargo.toml")
        .is_file()
    {
        return Some("effectcraft");
    }
    None
}

/// Why `dir` cannot be patched, in words that help a person who pointed the tool at the
/// wrong folder. The commonest mistake is a ready-to-run portable build: it holds the
/// compiled application, while these patches edit source code.
fn not_a_release(dir: &Path) -> String {
    let build = ["filmcraft.exe", "effectcraft.exe"]
        .iter()
        .find(|name| dir.join(name).is_file())
        .map(|name| name.trim_end_matches(".exe").to_string());

    if let Some(product) = build {
        return format!(
            "{} holds the compiled {product}, not its source code.\n\n\
             These patches change source files (.rs, .tsv, .md) and the application has to be \
             rebuilt afterwards, so a portable build or an installed copy cannot be patched \
             directly — there is nothing here to patch.\n\n\
             To get a portable build with these features:\n\
             1. patch the matching source folder (the one with apps/{product}/Cargo.toml),\n\
             2. run `cargo build --release` in it,\n\
             3. copy target/release/{product}.exe into this portable folder, keeping the old \
             file as a backup.\n\n\
             Nothing was written.",
            show(dir)
        );
    }

    format!(
        "{} is neither a FilmCraft nor an EffectCraft source folder.\n\n\
         Expected a folder that contains apps/filmcraft/Cargo.toml or \
         apps/effectcraft/Cargo.toml — the release sources as unpacked from the official \
         archive, not an installed or portable copy of the application.\n\n\
         Nothing was written.",
        show(dir)
    )
}

/// The name `FILES.txt` uses for a product (`filmcraft` -> `filmcraft-0.20.1`).
/// These must agree or the sha256 verification would compare against nothing and
/// report success for every file.
fn manifest_product(product: &str) -> &'static str {
    match product {
        "filmcraft" => "filmcraft-0.20.1",
        _ => "effectcraft-0.24.1",
    }
}

fn patches_for(product: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    PATCHES
        .iter()
        .filter(|(_, _, p, _)| *p == product)
        .map(|(id, name, _, text)| (*id, *name, *text))
        .collect()
}

/// Read a source file exactly as the patch expects it: UTF-8, LF line endings.
fn read_text(path: &Path) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", show(path)))?;
    let bytes = bytes
        .iter()
        .copied()
        .collect::<Vec<u8>>();
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{}: not valid UTF-8", show(path)))?;
    Ok(Some(text.replace("\r\n", "\n").replace('\r', "\n")))
}

/// A file the patch will write, with the text it should end up containing.
struct Plan {
    rel: String,
    text: String,
    existed: bool,
}

fn plan(dir: &Path, product: &str) -> Result<Vec<Plan>> {
    let mut out = Vec::new();
    let release = manifest_product(product);
    for (_, name, text) in patches_for(product) {
        let sections =
            patch::parse(text).map_err(|e| format!("{name}: the embedded patch is corrupt ({e})"))?;
        for section in sections {
            let dest = patch::destination(&section)
                .ok_or_else(|| format!("{name}: a section deletes a file; this tool refuses to"))?;
            refuse_escape(dest)?;

            let src_path = patch::source(&section)
                .map(|s| dir.join(s.replace('/', std::path::MAIN_SEPARATOR_STR)));
            let original = match &src_path {
                Some(p) => read_text(p)?,
                None => None,
            };

            let label = format!("{name} -> {dest}");
            let new_text = patch::apply(original.as_deref(), &section.hunks, &label).map_err(|e| {
                format!(
                    "{e}\n\nThis folder does not match the {release} release these patches were \
                     made against.\nUse the folder that contains apps/{product}/Cargo.toml from \
                     {release} and nothing else.\nNothing was written."
                )
            })?;

            out.push(Plan {
                rel: dest.to_string(),
                text: new_text,
                existed: original.is_some(),
            });
        }
    }
    Ok(out)
}

fn refuse_escape(rel: &str) -> Result<()> {
    if rel.starts_with('/') || rel.contains(':') {
        return Err(format!("patch path `{rel}` is absolute; refusing"));
    }
    if rel.split('/').any(|part| part == "..") {
        return Err(format!("patch path `{rel}` climbs out of the folder; refusing"));
    }
    Ok(())
}

fn check(dir: &Path) -> Result<()> {
    let dir = joinable(dir)?;
    let product = detect(&dir).ok_or_else(|| not_a_release(&dir))?;

    println!("Folder : {}", show(&dir));
    println!("Release: {product}\n");

    let plans = plan(&dir, product)?;
    let mut new_files = 0;
    let mut changed = 0;
    for p in &plans {
        if p.existed {
            changed += 1;
        } else {
            new_files += 1;
        }
    }

    println!(
        "Every context line matches: {} file(s) to write ({changed} replaced, {new_files} new).",
        plans.len()
    );
    for p in &plans {
        let tag = if p.existed { "replace" } else { "new    " };
        println!("  {tag} {}", p.rel);
    }
    println!("\nNothing was written. Run `apply` to patch this folder.");
    Ok(())
}

fn apply(dir: &Path, ask: bool) -> Result<()> {
    let dir = joinable(dir)?;
    let product = detect(&dir).ok_or_else(|| not_a_release(&dir))?;

    // Plan first: nothing is touched until every file has been proven to match.
    let plans = plan(&dir, product)?;
    println!("Folder : {}", show(&dir));
    println!("Release: {product}");
    println!(
        "Check  : every context line matches; {} file(s) to write.",
        plans.len()
    );

    if ask && !confirm("Apply the patches now? [y/N] ")? {
        println!("Cancelled; nothing was written.");
        return Ok(());
    }

    let stamp = timestamp();
    let backup_root = dir.join(BACKUP_DIR).join(&stamp);
    let mut backed_up = 0usize;
    let mut created: Vec<String> = Vec::new();

    for p in &plans {
        let dest = dir.join(p.rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", show(parent)))?;
        }

        if p.existed {
            let rel_backup = backup_root.join(p.rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Some(parent) = rel_backup.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", show(parent)))?;
            }
            fs::copy(&dest, &rel_backup)
                .map_err(|e| format!("cannot back up {}: {e}", show(&dest)))?;
            backed_up += 1;
        } else {
            created.push(p.rel.clone());
        }

        fs::write(&dest, p.text.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", show(&dest)))?;
    }

    // Record what was created, so `uninstall` can delete exactly those files and no
    // more. A backup alone cannot express "this file did not exist before".
    if !created.is_empty() {
        write_created_list(&backup_root, &created)?;
    }

    // Prove the result: compare what is on disk with FILES.txt and with the patch.
    const ROOT_DOCS: [&str; 2] = ["CRAFTLINK_ARCHITECTURE.md", "DYNAMIC_LINK_INTEGRATION.md"];
    let rows = manifest_rows();
    let product_key = manifest_product(product);
    let mut verified = 0usize;
    let mut docs = 0usize;
    let mut mismatched = Vec::new();
    for p in &plans {
        let dest = dir.join(p.rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let bytes = fs::read(&dest).map_err(|e| format!("cannot re-read {}: {e}", show(&dest)))?;
        if ROOT_DOCS.contains(&p.rel.as_str()) {
            docs += 1;
            continue;
        }
        let row = rows
            .iter()
            .find(|r| r.product == product_key && r.path == p.rel);
        match row {
            Some(row) => {
                let digest = sha256::hex16(&bytes);
                if bytes.len() == row.bytes && digest == row.digest {
                    verified += 1;
                } else {
                    mismatched.push(format!(
                        "{} (expected {} bytes / {}, wrote {} bytes / {})",
                        p.rel,
                        row.bytes,
                        row.digest,
                        bytes.len(),
                        digest
                    ));
                }
            }
            None => mismatched.push(format!("{} (not listed in FILES.txt)", p.rel)),
        }
    }

    println!("\nWrote {} file(s).", plans.len());
    if backed_up > 0 {
        println!(
            "Backed up {backed_up} replaced file(s) to {}",
            show(&backup_root)
        );
    }
    if !created.is_empty() {
        println!(
            "Recorded {} new file(s) in that folder, so `uninstall` can remove them.",
            created.len()
        );
    }
    println!("Verified against FILES.txt: {verified} file(s) ok{}.", {
        if docs > 0 {
            format!(", plus {docs} root document(s) the manifest does not list")
        } else {
            String::new()
        }
    });

    if !mismatched.is_empty() {
        println!("\nMISMATCH — these files do not match the published manifest:");
        for m in &mismatched {
            println!("  {m}");
        }
        println!(
            "This can only mean the folder had been edited before patching. \
             Restore the backup with `revert`."
        );
        return Err("verification failed".to_string());
    }

    println!(
        "\nDone. Next:\n  cargo build --release\n  cargo test -p {product}-engine craftlink"
    );
    Ok(())
}

/// Name of the file, inside a backup folder, that lists the files the patch created.
const CREATED_LIST: &str = "CREATED.txt";

fn write_created_list(backup_root: &Path, created: &[String]) -> Result<()> {
    fs::create_dir_all(backup_root)
        .map_err(|e| format!("cannot create {}: {e}", show(backup_root)))?;
    let mut text = String::from(
        "# Files this run created, relative to the release folder. `uninstall` deletes them.\n",
    );
    for rel in created {
        text.push_str(rel);
        text.push('\n');
    }
    let path = backup_root.join(CREATED_LIST);
    fs::write(&path, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", show(&path)))
}

/// The files a backup says were created by that run, if it recorded them.
fn read_created_list(backup_root: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(backup_root.join(CREATED_LIST)).ok()?;
    Some(
        text.lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .map(|line| line.trim().to_string())
            .collect(),
    )
}

/// Undo a patch run: restore the overwritten files, and with `remove_created` also
/// delete the files the run added, so the tree returns to its original state.
fn revert(dir: &Path, remove_created: bool) -> Result<()> {
    let dir = joinable(dir)?;
    let root = dir.join(BACKUP_DIR);
    if !root.is_dir() {
        return Err(format!(
            "no {} folder in {}; nothing to revert.\n\
             This folder has never been patched, or the backup was deleted.",
            BACKUP_DIR,
            show(&dir)
        ));
    }
    let newest = newest_backup(&root).ok_or_else(|| {
        format!("{} holds no backups; nothing to revert", show(&root))
    })?;

    let mut restored = 0usize;
    for entry in walk(&newest)? {
        if entry.file_name().map(|n| n == CREATED_LIST).unwrap_or(false) {
            continue;
        }
        let rel = entry
            .strip_prefix(&newest)
            .map_err(|_| "internal: backup path mismatch".to_string())?;
        let dest = dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", show(parent)))?;
        }
        fs::copy(&entry, &dest)
            .map_err(|e| format!("cannot restore {}: {e}", show(&dest)))?;
        restored += 1;
    }

    println!(
        "Restored {restored} file(s) from {}",
        newest.file_name().unwrap_or_default().to_string_lossy()
    );

    if !remove_created {
        println!(
            "Files the patch added are left in place; `uninstall` removes them as well."
        );
        return Ok(());
    }

    let mut removed = 0usize;
    let mut skipped = 0usize;
    match read_created_list(&newest) {
        None => {
            println!(
                "This backup carries no list of added files (it was made by an older build),\n\
                 so nothing was deleted. Compare the folder with a fresh release archive."
            );
        }
        Some(list) => {
            for rel in list {
                let path = dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                if !path.is_file() {
                    continue;
                }
                // Only delete a file that is exactly the one this patcher writes, so a
                // file the user has since edited is never thrown away silently.
                let expected = expected_text(&dir, &rel);
                let current = fs::read(&path).ok();
                let matches = match (&expected, &current) {
                    (Some(want), Some(have)) => want == have,
                    _ => false,
                };
                if matches {
                    fs::remove_file(&path)
                        .map_err(|e| format!("cannot delete {}: {e}", show(&path)))?;
                    removed += 1;
                } else {
                    skipped += 1;
                    println!("  left in place (changed since patching): {rel}");
                }
            }
            prune_empty_dirs(&dir)?;
            println!("Removed {removed} file(s) the patch had added.");
            if skipped > 0 {
                println!("{skipped} added file(s) were edited afterwards and were kept.");
            }
        }
    }

    println!("\nThe source folder is back to its original state. Rebuild with `cargo build --release`.");
    Ok(())
}

/// What the embedded patches would write to `rel`, or `None` if they do not write it.
fn expected_text(dir: &Path, rel: &str) -> Option<Vec<u8>> {
    let product = detect(dir)?;
    for (_, _, text) in patches_for(product) {
        let sections = patch::parse(text).ok()?;
        for section in sections {
            if patch::destination(&section) != Some(rel) {
                continue;
            }
            let original = patch::source(&section)
                .and_then(|src| read_text(&dir.join(src.replace('/', std::path::MAIN_SEPARATOR_STR))).ok().flatten());
            let out = patch::apply(original.as_deref(), &section.hunks, rel).ok()?;
            return Some(out.into_bytes());
        }
    }
    None
}

/// Delete folders that became empty, deepest first, stopping at the release root.
fn prune_empty_dirs(root: &Path) -> Result<()> {
    fn visit(dir: &Path) -> Result<bool> {
        let mut empty = true;
        for entry in fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", show(&dir)))? {
            let path = entry
                .map_err(|e| format!("cannot list {}: {e}", show(&dir)))?
                .path();
            if path.is_dir() {
                if visit(&path)? {
                    let _ = fs::remove_dir(&path);
                } else {
                    empty = false;
                }
            } else {
                empty = false;
            }
        }
        Ok(empty)
    }
    visit(root)?;
    Ok(())
}

fn newest_backup(root: &Path) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop()
}

fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)
            .map_err(|e| format!("cannot list {}: {e}", show(&dir)))?
        {
            let path = entry
                .map_err(|e| format!("cannot list {}: {e}", show(&dir)))?
                .path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

fn interactive(start: Option<&Path>) -> Result<()> {
    println!("CraftLink + 3D layer styles — patcher");
    println!("====================================\n");
    list();
    println!();
    println!("This edits a copy of the application's source folder. It installs nothing and");
    println!("does not touch the applications you already have installed.\n");
    println!("Редактируется копия папки с исходниками приложения. Ничего не устанавливается,");
    println!("уже установленные приложения не затрагиваются.\n");

    let dir = match start {
        Some(path) => joinable(path)?,
        None => {
            let answer = prompt(
                "Path to the FilmCraft or EffectCraft source folder (drag it here)\n\
                 Путь к папке исходников FilmCraft или EffectCraft (перетащите её сюда): ",
            )?;
            if answer.is_empty() {
                println!("Nothing entered; exiting.");
                return Ok(());
            }
            joinable(Path::new(&answer))?
        }
    };

    let product = detect(&dir).ok_or_else(|| not_a_release(&dir))?;
    println!("\nDetected: {product} in {}\n", show(&dir));

    let patched = is_patched(&dir, product)?;
    if patched {
        println!("This folder already looks patched. / Эта папка уже выглядит пропатченной.\n");
        println!("  1  uninstall — put the folder back as it was / вернуть как было");
        println!("  2  check     — show what would change / показать, что изменится");
        println!("  3  quit      — выход");
        match prompt("\nChoose [1/2/3]: ")?.trim() {
            "1" => {
                println!();
                return revert(&dir, true);
            }
            "2" => {
                println!();
                return check(&dir);
            }
            _ => {
                println!("Nothing was written. / Ничего не записано.");
                return Ok(());
            }
        }
    }

    let plans = plan(&dir, product)?;
    let replaced = plans.iter().filter(|p| p.existed).count();
    println!(
        "Check: every line of the original files matches. {} file(s) would be written \
         ({replaced} replaced, {} new).",
        plans.len(),
        plans.len() - replaced
    );
    println!(
        "Проверка: все строки исходных файлов совпали. Будет записано {} файл(ов) \
         ({replaced} замен, {} новых).",
        plans.len(),
        plans.len() - replaced
    );
    for p in &plans {
        println!("  {} {}", if p.existed { "replace" } else { "new    " }, p.rel);
    }

    println!("\n  1  apply — write these files (a backup is made first) / записать (сначала бэкап)");
    println!("  2  quit  — выход");
    if prompt("\nChoose [1/2]: ")?.trim() != "1" {
        println!("Nothing was written. / Ничего не записано.");
        return Ok(());
    }
    println!();
    apply(&dir, false)
}

/// Does the folder already carry these patches? A new file being present is enough,
/// and so is a replaced file that no longer matches the release.
fn is_patched(dir: &Path, product: &str) -> Result<bool> {
    for (_, name, text) in patches_for(product) {
        let sections = patch::parse(text)
            .map_err(|e| format!("{name}: the embedded patch is corrupt ({e})"))?;
        for section in sections {
            let Some(dest) = patch::destination(&section) else {
                continue;
            };
            let path = dir.join(dest.replace('/', std::path::MAIN_SEPARATOR_STR));
            if patch::source(&section).is_none() {
                if path.is_file() {
                    return Ok(true);
                }
                continue;
            }
            // A replaced file: patched if its content is what the patch would write.
            if let Some(want) = expected_text(dir, dest) {
                if fs::read(&path).ok().as_deref() == Some(want.as_slice()) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn joinable(dir: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(dir).map_err(|e| format!("cannot open `{}`: {e}", show(&dir)))?;
    if !path.is_dir() {
        return Err(format!("`{}` is not a folder", show(&path)));
    }
    Ok(path)
}

/// A path as a person wants to read it: Windows' `\\?\` prefix is an internal
/// spelling and only makes the output look broken.
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

/// Dragging a folder into a console wraps it in quotes and may add whitespace.
fn trim_input(raw: &str) -> &str {
    raw.trim().trim_matches('"').trim_matches('\'').trim()
}

/// A readable, sortable folder name, e.g. `2026-11-17_153045`.
///
/// A console user has to be able to find the backup by name, so this is a real
/// calendar date rather than a day count. The civil-from-days conversion is Howard
/// Hinnant's, which is exact for every date these tools will see.
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
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_patch_parses() {
        for (id, name, _, text) in PATCHES {
            let sections = patch::parse(text)
                .unwrap_or_else(|e| panic!("patch {id} ({name}) does not parse: {e}"));
            assert!(!sections.is_empty(), "patch {id} has no sections");
        }
    }

    #[test]
    fn the_manifest_matches_the_patches() {
        let rows = manifest_rows();
        assert_eq!(rows.len(), 45, "FILES.txt must list 45 files");

        for (id, name, product, text) in PATCHES {
            let key = manifest_product(product);
            let sections = patch::parse(text).expect("patch parses");
            for section in &sections {
                let Some(dest) = patch::destination(section) else {
                    panic!("patch {id} deletes a file");
                };
                if dest.ends_with(".md") {
                    continue; // the two shared documents are not in FILES.txt
                }
                let found = rows.iter().any(|r| r.product == key && r.path == dest);
                assert!(
                    found,
                    "patch {id} ({name}) ships {dest}, which FILES.txt does not list"
                );
            }
        }
    }

    #[test]
    fn the_patches_reproduce_the_manifest_hashes() {
        // Every new file must hash to its published bytes, and every replacement must
        // be listed with the size the patch would write. Catches a stale embed.
        let rows = manifest_rows();
        for (id, _, product, text) in PATCHES {
            let key = manifest_product(product);
            for section in patch::parse(text).expect("patch parses") {
                let Some(dest) = patch::destination(&section) else {
                    continue;
                };
                if dest.ends_with(".md") || patch::source(&section).is_some() {
                    continue; // docs are not listed; replacements need the release file
                }
                let out = patch::apply(None, &section.hunks, dest).expect("a new file applies");
                let Some(row) = rows.iter().find(|r| r.product == key && r.path == dest) else {
                    continue;
                };
                assert_eq!(
                    out.len(),
                    row.bytes,
                    "patch {id}: {dest} would be {} bytes, FILES.txt says {}",
                    out.len(),
                    row.bytes
                );
                assert_eq!(
                    sha256::hex16(out.as_bytes()),
                    row.digest,
                    "patch {id}: {dest} would not match the published sha256"
                );
            }
        }
    }

    #[test]
    fn the_date_is_a_real_calendar_date() {
        // The epoch, a known date, and a leap day.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_322), (2025, 8, 22));
    }

    #[test]
    fn a_portable_build_is_explained_not_just_refused() {
        let dir = std::env::temp_dir().join("craftlink-portable-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the sandbox is created");

        // A folder with a compiled exe in it is a build, not sources.
        fs::write(dir.join("effectcraft.exe"), b"stub").expect("the stub exe is written");
        let text = not_a_release(&dir);
        assert!(
            text.contains("compiled effectcraft"),
            "the message should say what it found: {text}"
        );
        assert!(
            text.contains("cargo build --release"),
            "the message should say how to get a portable build: {text}"
        );
        assert!(
            text.contains("Nothing was written"),
            "the message should promise no writes: {text}"
        );

        // An empty folder falls back to the generic explanation.
        let empty = not_a_release(&std::env::temp_dir());
        assert!(
            empty.contains("Cargo.toml"),
            "the generic message should name the file it looks for: {empty}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_backup_records_the_files_it_created() {
        let stamp = std::env::temp_dir().join("craftlink-created-test");
        let _ = fs::remove_dir_all(&stamp);
        let created = vec![
            "CRAFTLINK_ARCHITECTURE.md".to_string(),
            "crates/engine/src/craftlink_bridge.rs".to_string(),
        ];
        write_created_list(&stamp, &created).expect("the list is written");

        let read = read_created_list(&stamp).expect("the list is read back");
        assert_eq!(read, created, "the same names come back, in order");

        // A backup with no list must be distinguishable from one with an empty list.
        let empty = std::env::temp_dir().join("craftlink-created-test-empty");
        let _ = fs::remove_dir_all(&empty);
        write_created_list(&empty, &[]).expect("an empty list is written");
        assert_eq!(
            read_created_list(&empty),
            Some(Vec::new()),
            "an empty list is not a missing list"
        );

        let _ = fs::remove_dir_all(&stamp);
        let _ = fs::remove_dir_all(&empty);
    }
}