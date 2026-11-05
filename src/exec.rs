use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;

use crate::fix::{Rename, TagFix};
use crate::tags;
use crate::trash::Run;

#[derive(Default)]
pub struct Changes {
    pub tags: Vec<TagFix>,
    /// Applied in order, after the tag writes.
    pub renames: Vec<Rename>,
    /// Trashed at the end, and only if empty by then.
    pub remove_dirs: Vec<PathBuf>,
}

impl Changes {
    pub fn len(&self) -> usize {
        self.tags.len() + self.renames.len() + self.remove_dirs.len()
    }
}

pub struct Outcome {
    pub applied: usize,
    pub failed: Vec<String>,
    pub run_dir: Option<PathBuf>,
}

pub fn execute(library: &Path, changes: &Changes) -> Result<Outcome, String> {
    let run = Mutex::new(Run::start(library).map_err(|e| format!("No se pudo crear el registro: {e}"))?);
    // Tag writes may rewrite whole files over the network, so they go in parallel; the
    // journal entry is recorded under the lock before each write.
    let mut failed: Vec<String> = changes
        .tags
        .par_iter()
        .filter_map(|f| {
            let res = run.lock().unwrap().tags_begin(&f.rel, f.before.clone());
            res.and_then(|_| tags::write(&library.join(&f.rel), &f.after))
                .err()
                .map(|e| format!("{}: {e}", f.rel.display()))
        })
        .collect();
    let mut run = run.into_inner().unwrap();
    for r in &changes.renames {
        if let Err(e) = run.rename(&r.from, &r.to) {
            failed.push(format!("{}: {e}", r.from.display()));
        }
    }
    let mut skipped = 0;
    for d in &changes.remove_dirs {
        // Checked now, not when planned: a failed move leaves files behind.
        let empty = fs::read_dir(library.join(d)).map(|mut it| it.next().is_none()).unwrap_or(false);
        if !empty {
            skipped += 1;
        } else if let Err(e) = run.trash(d) {
            failed.push(format!("{}: {e}", d.display()));
        }
    }
    let applied = changes.len() - failed.len() - skipped;
    let run_dir = run.finish().map_err(|e| format!("No se pudo cerrar el registro: {e}"))?;
    Ok(Outcome { applied, failed, run_dir })
}
