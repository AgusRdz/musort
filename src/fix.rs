use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::doctor::nfd_paths;
use crate::scan::{Library, Track};
use crate::tags::Edit;
use crate::trash::same_file;
use crate::text::{clean_title, fold, is_junk, nfc, sanitize_filename, split_artists, strip_junk};

pub struct TagFix {
    pub rel: PathBuf,
    pub before: Edit,
    pub after: Edit,
}

pub struct Rename {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Default)]
pub struct Plan {
    pub tags: Vec<TagFix>,
    /// Applied in order: files, then Unicode-only renames (deepest first), then folders.
    pub files: Vec<Rename>,
    pub unicode: Vec<Rename>,
    pub folders: Vec<Rename>,
    pub review: Vec<String>,
}

struct Target<'a> {
    track: &'a Track,
    artists: Vec<String>,
    album_artist: String,
    title: String,
    album: String,
    comment: String,
}

fn name_of(p: &Path) -> String {
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// "07 - Title" in album folders keeps its track-number prefix.
fn track_prefix(t: &Track) -> &str {
    let stem = t.rel.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    if t.rel.components().count() < 3 {
        return "";
    }
    let digits = stem.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && stem[digits..].starts_with(" - ") { &stem[..digits + 3] } else { "" }
}

/// Folder names in the library, folded; collaborations that match one stay unsplit.
pub fn known_artists(lib: &Library) -> HashSet<String> {
    lib.tracks.iter().map(|t| fold(&t.folder())).collect()
}

/// The artists and album artist a track's tags should carry.
pub fn artist_target(t: &Track, known: &HashSet<String>) -> (Vec<String>, String) {
    let mut artists = split_artists(&t.artists, known);
    if artists.is_empty() {
        artists = t.artists.clone();
    }
    let album_artist = Some(strip_junk(&t.album_artist))
        .filter(|s| !s.is_empty())
        .or_else(|| artists.first().cloned())
        .unwrap_or_default();
    (artists, album_artist)
}

pub fn plan(lib: &Library, unicode: bool) -> Plan {
    let mut plan = Plan::default();
    let known = known_artists(lib);

    let mut by_folder: BTreeMap<String, Vec<Target>> = BTreeMap::new();
    for t in &lib.tracks {
        let (artists, album_artist) = artist_target(t, &known);
        let folder = t.folder();
        let mut names: Vec<&str> = artists.iter().map(String::as_str).collect();
        names.push(&folder);
        let title = Some(clean_title(&t.title, &names)).filter(|s| !s.is_empty()).unwrap_or(t.title.clone());

        // Tags that disagree with the folder need a human: the tag may be the wrong one.
        if fold(&album_artist) != fold(&folder) {
            plan.review.push(format!("{} — folder \"{folder}\", tag artist \"{album_artist}\"", t.rel.display()));
            continue;
        }
        let target = Target {
            track: t,
            artists,
            album_artist,
            title,
            album: strip_junk(&t.album),
            comment: if is_junk(&t.comment) { String::new() } else { t.comment.clone() },
        };
        by_folder.entry(folder).or_default().push(target);
    }

    for (folder, mut targets) in by_folder {
        // One spelling per folder: the most common one wins, ties go to the current folder name.
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for t in &targets {
            *counts.entry(t.album_artist.as_str()).or_default() += 1;
        }
        let canonical = counts
            .into_iter()
            .max_by_key(|(name, n)| (*n, sanitize_filename(name) == nfc(&folder)))
            .map(|(name, _)| name.to_owned())
            .unwrap_or_default();

        for t in &mut targets {
            t.album_artist = canonical.clone();
            for a in &mut t.artists {
                if fold(a) == fold(&canonical) {
                    *a = canonical.clone();
                }
            }
            push_tag_fix(&mut plan, t);
            push_file_rename(&mut plan, t);
        }

        let wanted = sanitize_filename(&canonical);
        if nfc(&folder) != wanted {
            plan.folders.push(Rename { from: PathBuf::from(&folder), to: PathBuf::from(wanted) });
        }
    }

    drop_collisions(lib, &mut plan);

    if unicode {
        let renamed: HashSet<&Path> =
            plan.files.iter().chain(&plan.folders).map(|r| r.from.as_path()).collect();
        let mut paths: Vec<PathBuf> =
            nfd_paths(lib).into_iter().filter(|p| !renamed.contains(p.as_path())).collect();
        paths.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        plan.unicode = paths
            .into_iter()
            .map(|p| {
                let to = p.with_file_name(nfc(&name_of(&p)));
                Rename { from: p, to }
            })
            .collect();
    }
    plan
}

fn push_tag_fix(plan: &mut Plan, t: &Target) {
    let src = t.track;
    let (mut before, mut after) = (Edit::default(), Edit::default());
    if t.title != src.title {
        before.title = Some(src.title.clone());
        after.title = Some(t.title.clone());
    }
    if t.album != src.album {
        before.album = Some(src.album.clone());
        after.album = Some(t.album.clone());
    }
    if t.artists != src.artists {
        before.artists = Some(src.artists.clone());
        after.artists = Some(t.artists.clone());
    }
    if t.album_artist != src.album_artist {
        before.album_artist = Some(src.album_artist.clone());
        after.album_artist = Some(t.album_artist.clone());
    }
    if t.comment != src.comment {
        before.comment = Some(src.comment.clone());
        after.comment = Some(t.comment.clone());
    }
    if !after.is_empty() {
        plan.tags.push(TagFix { rel: src.rel.clone(), before, after });
    }
}

fn push_file_rename(plan: &mut Plan, t: &Target) {
    let src = t.track;
    let ext = src.rel.extension().unwrap_or_default().to_string_lossy();
    let wanted = format!("{}{}", track_prefix(src), sanitize_filename(&t.title));
    if nfc(&src.stem()) == wanted || wanted.is_empty() {
        return;
    }
    let to = src.rel.with_file_name(format!("{wanted}.{ext}"));
    if src.has_lrc {
        plan.files.push(Rename { from: src.rel.with_extension("lrc"), to: to.with_extension("lrc") });
    }
    plan.files.push(Rename { from: src.rel.clone(), to });
}

/// A rename must not land on another file, whether it exists now or another rename creates it.
fn drop_collisions(lib: &Library, plan: &mut Plan) {
    let key = |p: &Path| p.components().map(|c| fold(&c.as_os_str().to_string_lossy())).collect::<Vec<_>>().join("/");
    let mut targets: HashMap<String, usize> = HashMap::new();
    for r in &plan.files {
        *targets.entry(key(&r.to)).or_default() += 1;
    }
    let mut bad = HashSet::new();
    for r in &plan.files {
        let (from, to) = (lib.root.join(&r.from), lib.root.join(&r.to));
        let taken_on_disk = to.exists() && !same_file(&from, &to);
        if targets[&key(&r.to)] > 1 || taken_on_disk {
            bad.insert(r.from.with_extension(""));
        }
    }
    plan.files.retain(|r| {
        let keep = !bad.contains(&r.from.with_extension(""));
        if !keep && r.from.extension().is_some_and(|e| e != "lrc") {
            plan.review.push(format!("{} — renaming to \"{}\" collides with another file", r.from.display(), name_of(&r.to)));
        }
        keep
    });

    // Folder renames only change case/accents/punctuation, so they clash only when another
    // folder already folds to the same name.
    let folders: HashSet<String> = lib.tracks.iter().map(|t| t.folder()).collect();
    plan.folders.retain(|r| {
        let ok = folders.iter().filter(|f| fold(f) == fold(&name_of(&r.to))).count() == 1;
        if !ok {
            plan.review.push(format!("{}/ — renaming to \"{}\" collides with another folder", r.from.display(), r.to.display()));
        }
        ok
    });
}
