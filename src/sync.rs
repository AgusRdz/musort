//! One-way mirror of the library's music and lyrics onto a player's card.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::scan::{AUDIO_EXT, JUNK_DIRS, JUNK_FILES};
use crate::text::{nfc, sanitize_filename};
use crate::trash::TRASH_DIR;

pub const MANIFEST: &str = ".musort-sync.json";
/// Android and FiiO folders at the card root that hold things other than this library.
const PROTECTED: &[&str] = &[
    "Android", "DCIM", "Music", "Movies", "Pictures", "Download", "Documents", "Alarms", "Notifications",
    "Podcasts", "Ringtones", "Audiobooks", "Recordings", "LOST.DIR", "System Volume Information", "fiio_log",
];
const MANIFEST_VERSION: u32 = 1;

/// Size and mtime of the library file a card file was copied from.
type Stamp = (u64, u64);

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    entries: HashMap<String, Stamp>,
}

pub struct CopyOp {
    pub src: PathBuf,
    /// Relative path on the card, in its final (sanitized, NFC) form.
    pub dest: String,
    stamp: Stamp,
}

pub struct Plan {
    pub copies: Vec<CopyOp>,
    /// Paths as listed on the card; directories are removed whole.
    pub deletes: Vec<PathBuf>,
    /// Lyrics only the card has (the player can download them) for songs the library has
    /// without lyrics: (card path, library path). Copied back instead of deleted.
    pub imports: Vec<(PathBuf, PathBuf)>,
    /// Not in the library but not music either, so left alone.
    pub kept: Vec<PathBuf>,
    pub unchanged: usize,
    pub bytes: u64,
    entries: HashMap<String, Stamp>,
}

fn stamp_of(m: &fs::Metadata) -> Stamp {
    let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    (m.len(), mtime)
}

fn key_of(rel: &Path) -> String {
    rel.components()
        .map(|c| nfc(&c.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

fn dest_key(src_rel: &Path) -> String {
    src_rel
        .components()
        .map(|c| sanitize_filename(&c.as_os_str().to_string_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

fn is_disposable(p: &Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
    hidden || ext == "lrc" || AUDIO_EXT.contains(&ext.as_str())
}

/// Only known junk; names like "...Baby One More Time.mp3" are real songs.
fn is_junk_file(p: &Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    name.starts_with("._") || JUNK_FILES.contains(&name.as_ref())
}

/// Music and lyrics in the library, with their stamps.
fn source_files(library: &Path) -> io::Result<Vec<(PathBuf, Stamp)>> {
    let mut paths = Vec::new();
    let walker = WalkDir::new(library).min_depth(1).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !(e.file_type().is_dir() && (name == TRASH_DIR || JUNK_DIRS.contains(&name.as_ref())))
    });
    for entry in walker {
        let entry = entry?;
        let rel = entry.path().strip_prefix(library).unwrap().to_path_buf();
        let ext = rel.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if entry.file_type().is_file() && !is_junk_file(&rel) && (AUDIO_EXT.contains(&ext.as_str()) || ext == "lrc") {
            paths.push(rel);
        }
    }
    paths
        .into_par_iter()
        .map(|rel| fs::metadata(library.join(&rel)).map(|m| (rel, stamp_of(&m))))
        .collect()
}

pub fn plan(library: &Path, dest: &Path) -> io::Result<Plan> {
    let sources = source_files(library)?;
    let manifest: HashMap<String, Stamp> = fs::read(dest.join(MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok())
        .filter(|m| m.version == MANIFEST_VERSION)
        .map(|m| m.entries)
        .unwrap_or_default();

    // What's on the card now, keyed by NFC path. Root-level hidden and system folders are skipped.
    let mut on_card: HashMap<String, (PathBuf, u64)> = HashMap::new();
    let mut card_dirs: Vec<PathBuf> = Vec::new();
    let walker = WalkDir::new(dest).min_depth(1).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        e.depth() > 1 || !(name.starts_with('.') || PROTECTED.contains(&name.as_ref()))
    });
    for entry in walker {
        let entry = entry?;
        let rel = entry.path().strip_prefix(dest).unwrap().to_path_buf();
        if entry.file_type().is_dir() {
            card_dirs.push(rel);
        } else {
            let size = entry.metadata()?.len();
            on_card.insert(key_of(&rel), (rel, size));
        }
    }

    let mut plan =
        Plan { copies: Vec::new(), deletes: Vec::new(), imports: Vec::new(), kept: Vec::new(), unchanged: 0, bytes: 0, entries: HashMap::new() };
    let mut wanted: HashSet<String> = HashSet::new();
    let mut audio_by_stem: HashMap<String, PathBuf> = HashMap::new();
    for (src, stamp) in sources {
        let key = dest_key(&src);
        wanted.insert(key.clone());
        if !key.ends_with(".lrc") {
            audio_by_stem.insert(key.rsplit_once('.').map_or(key.clone(), |(stem, _)| stem.to_owned()), src.clone());
        }
        let same = match (on_card.get(&key), manifest.get(&key)) {
            (Some((_, size)), Some(m)) => *m == stamp && *size == stamp.0,
            // First sync with this card: trust files that already have the right size.
            (Some((_, size)), None) => *size == stamp.0,
            (None, _) => false,
        };
        if same {
            plan.unchanged += 1;
            plan.entries.insert(key, stamp);
        } else {
            plan.bytes += stamp.0;
            plan.copies.push(CopyOp { src, dest: key, stamp });
        }
    }

    for (key, (rel, _)) in &on_card {
        let Some(stem) = key.strip_suffix(".lrc") else { continue };
        if let Some(audio) = audio_by_stem.get(stem).filter(|_| !wanted.contains(key)) {
            plan.imports.push((rel.clone(), audio.with_extension("lrc")));
        }
    }
    for (rel, _) in &plan.imports {
        wanted.insert(key_of(rel));
    }
    plan.imports.sort();

    // Folders that will hold nothing go whole; elsewhere only the stray files. Only music,
    // lyrics and hidden junk are ever deleted, so pointing at the wrong folder can't wipe it.
    let needed_dirs: HashSet<String> = wanted
        .iter()
        .flat_map(|k| {
            let parts: Vec<&str> = k.split('/').collect();
            (1..parts.len()).map(move |n| parts[..n].join("/"))
        })
        .collect();
    let stray: Vec<PathBuf> =
        on_card.into_iter().filter(|(k, _)| !wanted.contains(k)).map(|(_, (rel, _))| rel).collect();
    let (deletable, kept): (Vec<PathBuf>, Vec<PathBuf>) = stray.into_iter().partition(|p| is_disposable(p));
    card_dirs.sort();
    for d in card_dirs {
        let covered = plan.deletes.iter().any(|x| d.starts_with(x));
        let holds_kept = kept.iter().any(|k| k.starts_with(&d));
        if !covered && !holds_kept && !needed_dirs.contains(&key_of(&d)) {
            plan.deletes.push(d);
        }
    }
    let mut files: Vec<PathBuf> =
        deletable.into_iter().filter(|f| !plan.deletes.iter().any(|d| f.starts_with(d))).collect();
    files.sort();
    plan.deletes.extend(files);
    plan.kept = kept;
    plan.kept.sort();
    Ok(plan)
}

fn save_manifest(dest: &Path, entries: &HashMap<String, Stamp>) -> io::Result<()> {
    let m = Manifest { version: MANIFEST_VERSION, entries: entries.clone() };
    fs::write(dest.join(MANIFEST), serde_json::to_vec(&m)?)
}

/// Plain byte copy: `fs::copy` on macOS also writes extended attributes, which become
/// "._" files on FAT/exFAT cards.
fn copy_file(from: &Path, to: &Path, overwrite: bool) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut r = BufReader::with_capacity(1 << 20, File::open(from)?);
    let out = File::options().write(true).truncate(true).create(overwrite).create_new(!overwrite).open(to)?;
    let mut w = BufWriter::with_capacity(1 << 20, out);
    io::copy(&mut r, &mut w)?;
    w.flush()
}

/// Deletes first so a case-only rename on a case-insensitive card isn't deleted after copying.
pub fn apply(library: &Path, dest: &Path, mut plan: Plan, progress: impl Fn(usize, usize, &str)) -> Vec<String> {
    let mut failed = Vec::new();
    for (card, lib) in &plan.imports {
        // Never replaces lyrics the library already has.
        if let Err(e) = copy_file(&dest.join(card), &library.join(lib), false) {
            failed.push(format!("import lyrics {}: {e}", card.display()));
        }
    }
    for rel in &plan.deletes {
        if let Err(e) = remove(&dest.join(rel)) {
            failed.push(format!("delete {}: {e}", rel.display()));
        }
    }
    let total = plan.copies.len();
    for (i, c) in plan.copies.iter().enumerate() {
        progress(i + 1, total, &c.dest);
        match copy_file(&library.join(&c.src), &dest.join(&c.dest), true) {
            Ok(()) => {
                plan.entries.insert(c.dest.clone(), c.stamp);
            }
            Err(e) => {
                // A partial file must not look synced next time.
                let _ = fs::remove_file(dest.join(&c.dest));
                failed.push(format!("copy {}: {e}", c.dest));
            }
        }
        if (i + 1) % 50 == 0 {
            let _ = save_manifest(dest, &plan.entries);
        }
    }
    if let Err(e) = save_manifest(dest, &plan.entries) {
        failed.push(format!("save {MANIFEST}: {e}"));
    }
    if let Err(e) = remove_apple_double(dest) {
        failed.push(format!("clean up ._ files: {e}"));
    }
    failed
}

/// macOS lists exFAT names decomposed (NFD) but some entries, like "._" sidecars, only
/// resolve by their composed (NFC) spelling, so a miss is retried that way.
fn remove(p: &Path) -> io::Result<()> {
    let rm = |p: &Path| if p.is_dir() { fs::remove_dir_all(p) } else { fs::remove_file(p) };
    match rm(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let composed: PathBuf = p.components().map(|c| nfc(&c.as_os_str().to_string_lossy())).collect();
            if composed == p { Err(e) } else { rm(&composed) }
        }
        other => other,
    }
}

/// macOS tags every file a program creates with `com.apple.provenance`, which FAT/exFAT can
/// only store as a "._name" sidecar; it can't be avoided at creation, and deleting the sidecar
/// drops the attribute. Sweeps the card root and the music folders, never system folders.
fn remove_apple_double(dest: &Path) -> io::Result<()> {
    let walker = WalkDir::new(dest).min_depth(1).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        e.depth() > 1 || !e.file_type().is_dir() || !(name.starts_with('.') || PROTECTED.contains(&name.as_ref()))
    });
    for entry in walker {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name().to_string_lossy().starts_with("._") {
            // Removing one sidecar can take others with it, so a missing one is fine.
            match remove(entry.path()) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
    }
    Ok(())
}
