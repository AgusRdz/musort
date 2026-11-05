use std::path::PathBuf;

use crate::scan::Library;

pub struct Target {
    pub rel: PathBuf,
    pub reason: &'static str,
}

/// Everything `clean` would remove, without overlapping paths.
pub fn plan(lib: &Library) -> Vec<Target> {
    let groups: [(&Vec<PathBuf>, &'static str); 4] = [
        (&lib.junk_dirs, "system folder"),
        (&lib.dead_dirs, "folder without music"),
        (&lib.junk_files, "junk file"),
        (&lib.orphan_lrc, "lyrics without a song"),
    ];
    let all: Vec<(&PathBuf, &'static str)> =
        groups.iter().flat_map(|(paths, reason)| paths.iter().map(move |p| (p, *reason))).collect();
    all.iter()
        .filter(|(p, _)| !all.iter().any(|(o, _)| o != p && p.starts_with(o)))
        .map(|(p, reason)| Target { rel: (*p).clone(), reason })
        .collect()
}
