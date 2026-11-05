use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::config::Config;
use crate::scan::{Library, Track};
use crate::text::{base_title, fold, is_junk, is_normalized, nfc, primary_artist, sanitize_filename};

pub struct Check {
    pub title: &'static str,
    pub why: &'static str,
    pub items: Vec<String>,
}

fn show(p: &Path) -> String {
    p.display().to_string()
}

fn check(title: &'static str, why: &'static str, items: Vec<String>) -> Check {
    Check { title, why, items }
}

/// The artist a track's folder should be named after.
fn expected_artist(t: &Track) -> &str {
    if t.album_artist.is_empty() {
        t.artists.first().map_or("", |a| primary_artist(a))
    } else {
        &t.album_artist
    }
}

pub fn run(lib: &Library, cfg: &Config) -> Vec<Check> {
    let tracks = &lib.tracks;
    let list = |pred: &dyn Fn(&Track) -> bool| -> Vec<String> {
        tracks.iter().filter(|t| pred(t)).map(|t| show(&t.rel)).collect()
    };

    let mut checks = vec![
        check(
            "Archivos ilegibles",
            "No se pudieron leer los tags.",
            lib.errors.iter().map(|(p, e)| format!("{} — {e}", show(p))).collect(),
        ),
        check(
            "Sin album_artist",
            "Jellyfin agrupa artistas y álbumes por este campo; sin él usa `artist` completo.",
            list(&|t| t.album_artist.is_empty()),
        ),
        check(
            "Varios artistas en un solo valor",
            "Jellyfin no separa por coma: \"A, B\" se vuelve un artista nuevo.",
            tracks
                .iter()
                .filter(|t| t.artists.iter().any(|a| a.contains(',') || a.contains(';')))
                .map(|t| format!("{} — {}", show(&t.rel), t.artist_display()))
                .collect(),
        ),
        check(
            "Nombres guardados en NFD",
            "Cosmético: Jellyfin y el FiiO muestran el tag. `fix --unicode` los convierte, pero Jellyfin los verá como canciones nuevas.",
            nfd_paths(lib).iter().map(|p| show(p)).collect(),
        ),
        check(
            "Carpeta distinta al artista del tag",
            "La carpeta debería salir del tag (album_artist o artista principal).",
            {
                let mut m: BTreeMap<(String, String), usize> = BTreeMap::new();
                for t in tracks {
                    let want = sanitize_filename(expected_artist(t));
                    let have = sanitize_filename(&t.folder());
                    if want != have {
                        *m.entry((have, want)).or_default() += 1;
                    }
                }
                m.into_iter().map(|((have, want), n)| format!("{have}/ → \"{want}\" ({n})")).collect()
            },
        ),
        check(
            "Posibles alias de artista",
            "Carpetas que parecen el mismo artista con otro nombre.",
            alias_pairs(lib, cfg).into_iter().map(|(a, b)| format!("{a} ~ {b}")).collect(),
        ),
        check(
            "Nombre de archivo distinto al título",
            "El nombre debería salir del tag `title`.",
            tracks
                .iter()
                .filter(|t| {
                    let stem = sanitize_filename(&t.stem());
                    let title = sanitize_filename(&t.title);
                    // Album folders use "NN - Title".
                    stem != title && stem.split_once(" - ").map(|(_, s)| s) != Some(title.as_str())
                })
                .map(|t| format!("{} — \"{}\"", show(&t.rel), t.title))
                .collect(),
        ),
        check(
            "Basura en tags",
            "URLs o publicidad metidas en artista/álbum/título/comentario.",
            tracks
                .iter()
                .filter(|t| {
                    [&t.artist_display(), &t.album, &t.title, &t.album_artist, &t.comment].iter().any(|v| is_junk(v))
                })
                .map(|t| show(&t.rel))
                .collect(),
        ),
    ];

    let (dupes, same_title, versions) = duplicates(tracks);
    checks.push(check("Duplicados", "Mismo artista, título y álbum.", dupes));
    checks.push(check("Misma canción en otro álbum", "Puede ser intencional (en vivo, compilación).", same_title));
    checks.push(check("Versiones de una misma canción", "Remix, acústico, demo… normalmente intencional.", versions));

    checks.extend([
        check("Sin género", "No se puede navegar por género en Jellyfin ni en el FiiO.", list(&|t| t.genre.is_empty())),
        check("Sin año", "", list(&|t| t.year.is_none())),
        check("Sin carátula embebida", "", list(&|t| !t.has_art)),
        check("Sin letra (.lrc)", "", list(&|t| !t.has_lrc)),
        check("Letras huérfanas", ".lrc sin canción al lado.", lib.orphan_lrc.iter().map(|p| show(p)).collect()),
        check(
            "Carpetas sin música",
            "Vacías o creadas por otros dispositivos.",
            lib.dead_dirs.iter().map(|p| format!("{}/", show(p))).collect(),
        ),
        check(
            "Carpetas del sistema",
            "Creadas por macOS o Android.",
            lib.junk_dirs.iter().map(|p| format!("{}/", show(p))).collect(),
        ),
        check("Archivos basura", "", lib.junk_files.iter().map(|p| show(p)).collect()),
        check("Otros archivos", "", lib.other_files.iter().map(|p| show(p)).collect()),
    ]);

    checks.retain(|c| !c.items.is_empty());
    checks
}

/// Paths whose name is stored decomposed. macOS lists every name as NFD over SMB, so the only
/// reliable test is whether the NFC spelling resolves; that's also how `fix --unicode` finds them.
pub fn nfd_paths(lib: &Library) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    for t in &lib.tracks {
        let lrc = t.rel.with_extension("lrc");
        let paths = t.rel.ancestors().chain(if t.has_lrc { Some(lrc.as_path()) } else { None });
        for p in paths.filter(|a| !a.as_os_str().is_empty()) {
            if !is_normalized(&p.file_name().unwrap_or_default().to_string_lossy()) {
                candidates.insert(p.to_path_buf());
            }
        }
    }
    candidates
        .into_par_iter()
        .filter(|p| {
            let name = nfc(&p.file_name().unwrap_or_default().to_string_lossy());
            fs::symlink_metadata(lib.root.join(p.with_file_name(name))).is_err()
        })
        .collect()
}

/// Folder pairs that look like one artist under two names, minus the ones marked distinct.
pub fn alias_pairs(lib: &Library, cfg: &Config) -> Vec<(String, String)> {
    let folders: BTreeSet<String> = lib.tracks.iter().map(|t| t.folder()).collect();
    let mut out = Vec::new();
    for a in &folders {
        for b in &folders {
            let (fa, fb) = (fold(a), fold(b));
            let same = fa == fb && a < b;
            let prefix = fa != fb && fa.chars().count() >= 5 && fb.starts_with(&fa);
            if (same || prefix) && !cfg.is_distinct(a, b) {
                out.push((a.clone(), b.clone()));
            }
        }
    }
    out
}

fn duplicates(tracks: &[Track]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut groups: BTreeMap<(String, String), Vec<&Track>> = BTreeMap::new();
    for t in tracks {
        groups.entry((fold(&t.folder()), base_title(&t.title))).or_default().push(t);
    }

    let (mut dupes, mut same_title, mut versions) = (Vec::new(), Vec::new(), Vec::new());
    for group in groups.values().filter(|g| g.len() > 1) {
        let mut by_title: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
        for t in group {
            by_title.entry(fold(&t.title)).or_default().push(t);
        }
        let fmt = |ts: &[&Track]| ts.iter().map(|t| show(&t.rel)).collect::<Vec<_>>().join("  |  ");
        for same in by_title.values().filter(|v| v.len() > 1) {
            let mut by_album: BTreeMap<String, Vec<&Track>> = BTreeMap::new();
            for t in same {
                by_album.entry(fold(&t.album)).or_default().push(t);
            }
            for exact in by_album.values().filter(|v| v.len() > 1) {
                dupes.push(fmt(exact));
            }
            if by_album.len() > 1 {
                same_title.push(fmt(same));
            }
        }
        if by_title.len() > 1 {
            versions.push(fmt(group));
        }
    }
    (dupes, same_title, versions)
}
