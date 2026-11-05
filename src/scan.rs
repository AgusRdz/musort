use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::config::ParseOptions;
use lofty::prelude::*;
use lofty::probe::Probe;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::trash::TRASH_DIR;

pub const AUDIO_EXT: &[&str] = &["mp3", "flac", "m4a", "ogg", "opus", "wav"];
pub const JUNK_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini", ".database_uuid", ".nomedia"];
/// Created by macOS or Android on whatever disk they touch.
pub const JUNK_DIRS: &[&str] = &[".Spotlight-V100", ".fseventsd", ".Trashes", ".TemporaryItems", ".thumbnails"];
const CACHE_VERSION: u32 = 2;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Track {
    /// Relative to the library root.
    pub rel: PathBuf,
    pub artists: Vec<String>,
    pub album_artist: String,
    pub album: String,
    pub title: String,
    pub track: Option<u32>,
    pub year: Option<u16>,
    pub genre: String,
    pub comment: String,
    pub has_art: bool,
    #[serde(skip)]
    pub has_lrc: bool,
}

impl Track {
    pub fn folder(&self) -> String {
        self.rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub fn artist_display(&self) -> String {
        self.artists.join(" / ")
    }

    pub fn stem(&self) -> String {
        self.rel.file_stem().unwrap_or_default().to_string_lossy().into_owned()
    }
}

#[derive(Debug, Default)]
pub struct Library {
    pub root: PathBuf,
    pub tracks: Vec<Track>,
    pub junk_files: Vec<PathBuf>,
    pub junk_dirs: Vec<PathBuf>,
    pub other_files: Vec<PathBuf>,
    pub orphan_lrc: Vec<PathBuf>,
    /// Directories with no audio and no other files below them (outermost only).
    pub dead_dirs: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
}

/// Size and mtime decide whether the cached tags are still valid.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
struct Stamp {
    size: u64,
    mtime: u64,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    entries: HashMap<PathBuf, (Stamp, Track)>,
}

fn cache_path(root: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(root).ok()?;
    let name: String = root
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    Some(dirs::cache_dir()?.join("musort").join(format!("{name}.json")))
}

fn load_cache(path: Option<&Path>) -> HashMap<PathBuf, (Stamp, Track)> {
    path.and_then(|p| fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
        .filter(|c| c.version == CACHE_VERSION)
        .map(|c| c.entries)
        .unwrap_or_default()
}

fn save_cache(path: Option<&Path>, entries: HashMap<PathBuf, (Stamp, Track)>) {
    let Some(path) = path else { return };
    let cache = Cache { version: CACHE_VERSION, entries };
    // A failed write only costs a slower next scan.
    if let (Some(dir), Ok(json)) = (path.parent(), serde_json::to_vec(&cache)) {
        let _ = fs::create_dir_all(dir).and_then(|_| fs::write(path, json));
    }
}

fn ext_of(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn scan(root: &Path) -> Library {
    let mut lib = Library { root: root.to_path_buf(), ..Default::default() };
    let mut audio: Vec<(PathBuf, Stamp)> = Vec::new();
    let mut audio_paths = Vec::new();
    let mut lrcs = Vec::new();
    let mut dirs = Vec::new();
    let mut junk_dirs = Vec::new();

    let walker = WalkDir::new(root).min_depth(1).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if e.depth() == 1 && name == TRASH_DIR {
            return false;
        }
        if e.file_type().is_dir() && JUNK_DIRS.contains(&name.as_ref()) {
            junk_dirs.push(e.path().strip_prefix(root).unwrap().to_path_buf());
            return false;
        }
        true
    });
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                lib.errors.push((e.path().map(Path::to_path_buf).unwrap_or_default(), e.to_string()));
                continue;
            }
        };
        let rel = entry.path().strip_prefix(root).unwrap().to_path_buf();
        if entry.file_type().is_dir() {
            dirs.push(rel);
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        let ext = ext_of(&rel);
        if entry.depth() == 1 && name == crate::config::FILE {
            continue;
        }
        if JUNK_FILES.contains(&name.as_ref()) || name.starts_with("._") {
            lib.junk_files.push(rel);
        } else if AUDIO_EXT.contains(&ext.as_str()) {
            audio_paths.push(rel);
        } else if ext == "lrc" {
            lrcs.push(rel);
        } else {
            lib.other_files.push(rel);
        }
    }
    lib.junk_dirs = junk_dirs;

    // One stat per file is a network round trip on SMB; issuing them in parallel is ~5x faster.
    let stats: Vec<_> = audio_paths
        .into_par_iter()
        .map(|rel| {
            let m = fs::metadata(root.join(&rel));
            (rel, m)
        })
        .collect();
    for (rel, m) in stats {
        match m {
            Ok(m) => {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                audio.push((rel, Stamp { size: m.len(), mtime }));
            }
            Err(e) => lib.errors.push((rel, e.to_string())),
        }
    }

    let lrc_set: HashSet<PathBuf> = lrcs.iter().cloned().collect();
    let audio_stems: HashSet<PathBuf> = audio.iter().map(|(p, _)| p.with_extension("")).collect();
    lib.orphan_lrc = lrcs.into_iter().filter(|l| !audio_stems.contains(&l.with_extension(""))).collect();

    // Anything we couldn't read counts as content, so its folder is never treated as dead.
    let keep_dirs: HashSet<&Path> = audio
        .iter()
        .map(|(p, _)| p.as_path())
        .chain(lib.other_files.iter().map(PathBuf::as_path))
        .flat_map(|p| p.ancestors().skip(1))
        .chain(lib.errors.iter().flat_map(|(p, _)| p.strip_prefix(root).unwrap_or(p).ancestors()))
        .collect();
    dirs.sort();
    for d in dirs {
        let covered = lib.dead_dirs.iter().any(|dead| d.starts_with(dead));
        if !covered && !keep_dirs.contains(d.as_path()) {
            lib.dead_dirs.push(d);
        }
    }

    let cache_file = cache_path(root);
    let cache = load_cache(cache_file.as_deref());
    let results: Vec<Result<(PathBuf, Stamp, Track), (PathBuf, String)>> = audio
        .into_par_iter()
        .map(|(rel, stamp)| match cache.get(&rel) {
            Some((s, t)) if *s == stamp => Ok((rel, stamp, t.clone())),
            _ => read_track(root, &rel).map(|t| (rel, stamp, t)),
        })
        .collect();

    let mut fresh = HashMap::new();
    for r in results {
        match r {
            Ok((rel, stamp, mut t)) => {
                fresh.insert(rel.clone(), (stamp, t.clone()));
                t.has_lrc = lrc_set.contains(&rel.with_extension("lrc"));
                lib.tracks.push(t);
            }
            Err(e) => lib.errors.push(e),
        }
    }
    save_cache(cache_file.as_deref(), fresh);
    lib.tracks.sort_by(|a, b| a.rel.cmp(&b.rel));
    lib
}

fn read_track(root: &Path, rel: &Path) -> Result<Track, (PathBuf, String)> {
    let err = |e: lofty::error::FileParseError| (rel.to_path_buf(), e.to_string());
    let file = Probe::open(root.join(rel))
        .map_err(err)?
        .options(ParseOptions::new().read_properties(false))
        .read()
        .map_err(err)?;

    let mut t = Track { rel: rel.to_path_buf(), ..Default::default() };
    if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
        let s = |v: Option<std::borrow::Cow<'_, str>>| v.map(|c| c.trim().to_owned()).unwrap_or_default();
        t.artists = tag.get_strings(ItemKey::TrackArtist).map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()).collect();
        t.album = s(tag.album());
        t.title = s(tag.title());
        t.genre = s(tag.genre());
        t.comment = s(tag.comment());
        t.album_artist = tag.get_string(ItemKey::AlbumArtist).unwrap_or_default().trim().to_owned();
        t.track = tag.track();
        t.year = tag.date().map(|d| d.year);
        t.has_art = !tag.pictures().is_empty();
    }
    Ok(t)
}
