//! Turns the decisions made in the review screen into tag edits and moves.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::doctor::alias_pairs;
use crate::exec::Changes;
use crate::fix::{artist_target, known_artists, Rename, TagFix};
use crate::scan::{Library, Track};
use crate::tags::Edit;
use crate::text::{fold, sanitize_filename};

/// A track whose tag names a different artist than its folder.
pub struct Mismatch {
    pub rel: PathBuf,
    pub folder: String,
    pub tag_artist: String,
}

pub struct Alias {
    pub a: String,
    pub b: String,
}

#[derive(Clone)]
pub enum Decision {
    UseFolder,
    UseTag,
    Artist(String),
}

#[derive(Clone)]
pub enum Merge {
    IntoA,
    IntoB,
    Name(String),
    Distinct,
}

pub fn mismatches(lib: &Library) -> Vec<Mismatch> {
    let known = known_artists(lib);
    lib.tracks
        .iter()
        .filter_map(|t| {
            let (_, album_artist) = artist_target(t, &known);
            (fold(&album_artist) != fold(&t.folder())).then(|| Mismatch {
                rel: t.rel.clone(),
                folder: t.folder(),
                tag_artist: album_artist,
            })
        })
        .collect()
}

pub fn aliases(lib: &Library, cfg: &Config) -> Vec<Alias> {
    alias_pairs(lib, cfg).into_iter().map(|(a, b)| Alias { a, b }).collect()
}

pub fn tracks_in<'a>(lib: &'a Library, folder: &str) -> Vec<&'a Track> {
    lib.tracks.iter().filter(|t| t.folder() == folder).collect()
}

/// How the artist is spelled in the tags of a folder's tracks; the folder name has lost
/// characters like "/" ("AC DC" for "AC/DC").
pub fn display_name(lib: &Library, folder: &str) -> String {
    let known = known_artists(lib);
    let mut counts: HashMap<String, usize> = HashMap::new();
    for t in tracks_in(lib, folder) {
        let (_, aa) = artist_target(t, &known);
        if fold(&aa) == fold(folder) {
            *counts.entry(aa).or_default() += 1;
        }
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map_or_else(|| folder.to_owned(), |(name, _)| name)
}

/// The artist list a track ends up with when `new` replaces its current main artist.
pub fn reassigned_artists(lib: &Library, t: &Track, new: &str) -> Vec<String> {
    let (mut artists, old) = artist_target(t, &known_artists(lib));
    let mut replaced = false;
    for a in &mut artists {
        if fold(a) == fold(&old) {
            *a = new.to_owned();
            replaced = true;
        }
    }
    if !replaced {
        match artists.first_mut() {
            Some(first) => *first = new.to_owned(),
            None => artists.push(new.to_owned()),
        }
    }
    let mut seen = HashSet::new();
    artists.retain(|a| seen.insert(fold(a)));
    artists
}

/// An existing folder for this artist if there is one, so "Ac/Dc" lands in "AC DC".
pub fn folder_for(lib: &Library, artist: &str) -> String {
    let wanted = sanitize_filename(artist);
    lib.tracks
        .iter()
        .map(|t| t.folder())
        .find(|f| fold(f) == fold(&wanted))
        .unwrap_or(wanted)
}

struct Builder<'a> {
    lib: &'a Library,
    changes: Changes,
    folder_renames: Vec<Rename>,
    moves: Vec<Rename>,
    done: HashSet<PathBuf>,
}

impl Builder<'_> {
    /// Retags a track under `artist` and moves it into `folder` if it lives elsewhere.
    fn assign(&mut self, t: &Track, artist: &str, folder: &str) {
        if !self.done.insert(t.rel.clone()) {
            return;
        }
        let artists = reassigned_artists(self.lib, t, artist);
        let (mut before, mut after) = (Edit::default(), Edit::default());
        if artists != t.artists {
            before.artists = Some(t.artists.clone());
            after.artists = Some(artists);
        }
        if artist != t.album_artist {
            before.album_artist = Some(t.album_artist.clone());
            after.album_artist = Some(artist.to_owned());
        }
        if !after.is_empty() {
            self.changes.tags.push(TagFix { rel: t.rel.clone(), before, after });
        }
        if fold(&t.folder()) != fold(folder) {
            let inner: PathBuf = t.rel.components().skip(1).collect();
            let to = Path::new(folder).join(&inner);
            if t.has_lrc {
                self.moves.push(Rename { from: t.rel.with_extension("lrc"), to: to.with_extension("lrc") });
            }
            self.moves.push(Rename { from: t.rel.clone(), to });
            // Removed only if the move left it empty.
            let source = PathBuf::from(t.folder());
            if !self.changes.remove_dirs.contains(&source) {
                self.changes.remove_dirs.push(source);
            }
        }
    }

    /// Moves every track of `from` under `artist`; a folder whose name only differs in
    /// case/accents is renamed instead of emptied.
    fn merge_folder(&mut self, from: &str, artist: &str, folder: &str) {
        for t in tracks_in(self.lib, from) {
            self.assign(t, artist, folder);
        }
        if fold(from) == fold(folder) && from != folder {
            self.folder_renames.push(Rename { from: from.into(), to: folder.into() });
        }
    }
}

/// Returns the changes plus the alias pairs to remember as distinct.
pub fn plan(
    lib: &Library,
    decisions: &[(&Mismatch, Decision)],
    merges: &[(&Alias, Merge)],
) -> (Changes, Vec<(String, String)>) {
    let mut b = Builder { lib, changes: Changes::default(), folder_renames: Vec::new(), moves: Vec::new(), done: HashSet::new() };
    let mut distinct = Vec::new();

    for (m, d) in decisions {
        let Some(t) = lib.tracks.iter().find(|t| t.rel == m.rel) else { continue };
        let artist = match d {
            Decision::UseFolder => display_name(lib, &m.folder),
            Decision::UseTag => m.tag_artist.clone(),
            Decision::Artist(name) => name.clone(),
        };
        let folder = if matches!(d, Decision::UseFolder) { m.folder.clone() } else { folder_for(lib, &artist) };
        b.assign(t, &artist, &folder);
    }

    for (a, m) in merges {
        match m {
            Merge::Distinct => distinct.push((a.a.clone(), a.b.clone())),
            Merge::IntoA => b.merge_folder(&a.b, &display_name(lib, &a.a), &a.a),
            Merge::IntoB => b.merge_folder(&a.a, &display_name(lib, &a.b), &a.b),
            Merge::Name(name) => {
                // The typed name keeps its exact spelling, even over an existing folder.
                let folder = sanitize_filename(name);
                b.merge_folder(&a.a, name, &folder);
                b.merge_folder(&a.b, name, &folder);
            }
        }
    }

    // Folder renames first, so moves into a renamed folder find it under its new name.
    b.changes.renames = b.folder_renames.into_iter().chain(b.moves).collect();
    (b.changes, distinct)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(rel: &str, artists: &[&str], album_artist: &str) -> Track {
        Track {
            rel: rel.into(),
            artists: artists.iter().map(|a| a.to_string()).collect(),
            album_artist: album_artist.into(),
            has_lrc: true,
            ..Default::default()
        }
    }

    fn lib(tracks: Vec<Track>) -> Library {
        Library { tracks, ..Default::default() }
    }

    #[test]
    fn use_folder_replaces_wrong_artist_and_keeps_guests() {
        let l = lib(vec![
            track("Edwin Luna/Dos Monedas.mp3", &["BandaMx", "Pancho"], ""),
            track("Edwin Luna/Otra.mp3", &["Edwin Luna"], "Edwin Luna"),
        ]);
        let ms = mismatches(&l);
        assert_eq!(ms.len(), 1);
        let (c, _) = plan(&l, &[(&ms[0], Decision::UseFolder)], &[]);
        assert_eq!(c.tags[0].after.artists, Some(vec!["Edwin Luna".into(), "Pancho".into()]));
        assert_eq!(c.tags[0].after.album_artist.as_deref(), Some("Edwin Luna"));
        assert!(c.renames.is_empty());
    }

    #[test]
    fn use_tag_moves_track_and_lyrics_into_existing_folder() {
        let l = lib(vec![
            track("Ramon Ayala Y Sus Bravos/El Disgusto.mp3", &["Ramón Ayala"], ""),
            track("RAMÓN AYALA/Otra.mp3", &["Ramón Ayala"], "Ramón Ayala"),
        ]);
        let ms = mismatches(&l);
        let (c, _) = plan(&l, &[(&ms[0], Decision::UseTag)], &[]);
        let moves: Vec<_> = c.renames.iter().map(|r| r.to.to_string_lossy().into_owned()).collect();
        assert_eq!(moves, ["RAMÓN AYALA/El Disgusto.lrc", "RAMÓN AYALA/El Disgusto.mp3"]);
        assert_eq!(c.remove_dirs, [PathBuf::from("Ramon Ayala Y Sus Bravos")]);
    }

    #[test]
    fn merge_moves_folder_and_marks_it_for_removal() {
        let l = lib(vec![
            track("Banda El Recodo/A.mp3", &["Banda El Recodo"], "Banda El Recodo"),
            track("Banda El Recodo De Cruz Lizárraga/B.mp3", &["Banda El Recodo De Cruz Lizárraga", "Guest"], "Banda El Recodo De Cruz Lizárraga"),
        ]);
        let al = aliases(&l, &Config::default());
        assert_eq!(al.len(), 1);
        let (c, _) = plan(&l, &[], &[(&al[0], Merge::IntoB)]);
        assert_eq!(c.tags.len(), 1);
        assert_eq!(c.tags[0].after.artists, Some(vec!["Banda El Recodo De Cruz Lizárraga".into()]));
        assert_eq!(c.renames.last().unwrap().to, Path::new("Banda El Recodo De Cruz Lizárraga/A.mp3"));
        assert_eq!(c.remove_dirs, [PathBuf::from("Banda El Recodo")]);
    }

    #[test]
    fn typed_name_renames_the_matching_folder_instead_of_emptying_it() {
        let l = lib(vec![
            track("Millonario/A.mp3", &["Millonario"], "Millonario"),
            track("Millonario & W. Corona/B.mp3", &["Millonario & W. Corona"], "Millonario & W. Corona"),
        ]);
        let al = aliases(&l, &Config::default());
        let (c, _) = plan(&l, &[], &[(&al[0], Merge::Name("MILLONARIO".into()))]);
        assert_eq!(c.renames[0].from, Path::new("Millonario"));
        assert_eq!(c.renames[0].to, Path::new("MILLONARIO"));
        assert_eq!(c.remove_dirs, [PathBuf::from("Millonario & W. Corona")]);
    }

    #[test]
    fn distinct_pairs_are_not_asked_again() {
        let l = lib(vec![
            track("La Adictiva/A.mp3", &["La Adictiva"], "La Adictiva"),
            track("La Adictiva Banda San José/B.mp3", &["La Adictiva Banda San José"], "La Adictiva Banda San José"),
        ]);
        let mut cfg = Config::default();
        let al = aliases(&l, &cfg);
        let (c, distinct) = plan(&l, &[], &[(&al[0], Merge::Distinct)]);
        assert_eq!(c.len(), 0);
        cfg.mark_distinct(&distinct[0].0, &distinct[0].1);
        assert!(aliases(&l, &cfg).is_empty());
    }
}
