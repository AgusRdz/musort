//! Every change runs inside a journaled run under `.musort-trash/<timestamp>/` so `undo` can
//! reverse it: removed files are moved there instead of deleted, renames and tag edits are
//! recorded with their previous state. Jellyfin ignores dot-folders and `sync` skips this one.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::tags::{self, Edit};

pub const TRASH_DIR: &str = ".musort-trash";
const JOURNAL: &str = "journal.json";

#[derive(Debug, Serialize, Deserialize)]
pub enum Op {
    Trash { rel: PathBuf },
    Rename { from: PathBuf, to: PathBuf },
    Tags { rel: PathBuf, before: Edit },
}

pub struct Run {
    root: PathBuf,
    pub dir: PathBuf,
    ops: Vec<Op>,
}

/// True when both paths resolve to the same file, as with a case-only rename on a
/// case-insensitive share. Comparing names instead would let "Mi" overwrite a distinct "Mí".
pub fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

impl Run {
    pub fn start(root: &Path) -> io::Result<Run> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let dir = root.join(TRASH_DIR).join(stamp.to_string());
        fs::create_dir_all(&dir)?;
        Ok(Run { root: root.to_path_buf(), dir, ops: Vec::new() })
    }

    /// Written before the change it describes, so an interrupted run can still be undone.
    fn record(&mut self, op: Op) -> io::Result<()> {
        self.ops.push(op);
        fs::write(self.dir.join(JOURNAL), serde_json::to_vec_pretty(&self.ops)?)
    }

    fn forget_last(&mut self) -> io::Result<()> {
        self.ops.pop();
        fs::write(self.dir.join(JOURNAL), serde_json::to_vec_pretty(&self.ops)?)
    }

    pub fn trash(&mut self, rel: &Path) -> io::Result<()> {
        let dest = self.dir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        self.record(Op::Trash { rel: rel.to_path_buf() })?;
        fs::rename(self.root.join(rel), &dest).or_else(|e| {
            self.forget_last()?;
            Err(e)
        })
    }

    pub fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let dest = self.root.join(to);
        if dest.exists() && !same_file(&self.root.join(from), &dest) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!("ya existe {}", to.display())));
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        self.record(Op::Rename { from: from.to_path_buf(), to: to.to_path_buf() })?;
        fs::rename(self.root.join(from), &dest).or_else(|e| {
            self.forget_last()?;
            Err(e)
        })
    }

    /// Records the current values of the fields about to change; the caller then writes the
    /// tag. If that write fails, undoing it just rewrites the same values.
    pub fn tags_begin(&mut self, rel: &Path, before: Edit) -> Result<(), String> {
        self.record(Op::Tags { rel: rel.to_path_buf(), before }).map_err(|e| e.to_string())
    }

    /// Drops the run folder if nothing was done.
    pub fn finish(self) -> io::Result<Option<PathBuf>> {
        if self.ops.is_empty() {
            fs::remove_dir_all(&self.dir)?;
            remove_trash_if_empty(&self.root)?;
            return Ok(None);
        }
        Ok(Some(self.dir))
    }
}

fn remove_trash_if_empty(root: &Path) -> io::Result<()> {
    let trash = root.join(TRASH_DIR);
    if fs::read_dir(&trash)?.next().is_none() {
        fs::remove_dir(&trash)?;
    }
    Ok(())
}

fn read_journal(path: &Path) -> io::Result<Vec<Op>> {
    let bytes = fs::read(path)?;
    if let Ok(ops) = serde_json::from_slice::<Vec<Op>>(&bytes) {
        return Ok(ops);
    }
    // Runs from before the journal recorded anything but trashed paths.
    let rels: Vec<PathBuf> = serde_json::from_slice(&bytes)?;
    Ok(rels.into_iter().map(|rel| Op::Trash { rel }).collect())
}

pub struct Restored {
    pub run_dir: PathBuf,
    pub restored: usize,
    pub failed: Vec<String>,
}

/// Reverses the most recent run. Returns None if there is nothing to undo.
pub fn undo(root: &Path) -> io::Result<Option<Restored>> {
    let trash = root.join(TRASH_DIR);
    let latest = match fs::read_dir(&trash) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str()?.parse::<u64>().ok().map(|n| (n, e.path())))
            .filter(|(_, p)| p.join(JOURNAL).is_file())
            .max_by_key(|(n, _)| *n)
            .map(|(_, p)| p),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let Some(run_dir) = latest else { return Ok(None) };

    let ops = read_journal(&run_dir.join(JOURNAL))?;
    let mut out = Restored { run_dir: run_dir.clone(), restored: 0, failed: Vec::new() };
    let mut remaining = Vec::new();
    for op in ops.into_iter().rev() {
        let res = match &op {
            Op::Trash { rel } => {
                let dest = root.join(rel);
                if dest.exists() {
                    Err(format!("{}: ya existe", rel.display()))
                } else {
                    dest.parent()
                        .map_or(Ok(()), fs::create_dir_all)
                        .and_then(|_| fs::rename(run_dir.join(rel), &dest))
                        .map_err(|e| format!("{}: {e}", rel.display()))
                }
            }
            Op::Rename { from, to } => {
                if root.join(from).exists() && !same_file(&root.join(from), &root.join(to)) {
                    Err(format!("{}: ya existe", from.display()))
                } else {
                    fs::rename(root.join(to), root.join(from)).map_err(|e| format!("{}: {e}", to.display()))
                }
            }
            Op::Tags { rel, before } => {
                tags::write(&root.join(rel), before).map_err(|e| format!("{}: {e}", rel.display()))
            }
        };
        match res {
            Ok(()) => out.restored += 1,
            Err(e) => {
                out.failed.push(e);
                remaining.push(op);
            }
        }
    }

    if remaining.is_empty() {
        fs::remove_dir_all(&run_dir)?;
        remove_trash_if_empty(root)?;
    } else {
        // Keep only what wasn't reversed so a later undo can retry it.
        remaining.reverse();
        fs::write(run_dir.join(JOURNAL), serde_json::to_vec_pretty(&remaining)?)?;
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_rename_then_undo_restores_everything() {
        let root = std::env::temp_dir().join(format!("musort-trash-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("Empty Artist")).unwrap();
        fs::create_dir_all(root.join("Artist")).unwrap();
        fs::write(root.join("Artist/Song.lrc"), "x").unwrap();
        fs::write(root.join("Artist/Old.lrc"), "y").unwrap();

        let mut run = Run::start(&root).unwrap();
        run.trash(Path::new("Empty Artist")).unwrap();
        run.trash(Path::new("Artist/Song.lrc")).unwrap();
        run.rename(Path::new("Artist/Old.lrc"), Path::new("Artist/New.lrc")).unwrap();
        run.rename(Path::new("Artist"), Path::new("ARTIST")).unwrap();
        assert!(run.finish().unwrap().is_some());
        assert!(root.join("ARTIST/New.lrc").is_file());

        let r = undo(&root).unwrap().unwrap();
        assert_eq!((r.restored, r.failed.len()), (4, 0));
        assert!(root.join("Artist/Song.lrc").is_file() && root.join("Artist/Old.lrc").is_file());
        assert!(root.join("Empty Artist").is_dir());
        assert!(!root.join(TRASH_DIR).exists());
        assert!(undo(&root).unwrap().is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rename_refuses_to_overwrite() {
        let root = std::env::temp_dir().join(format!("musort-rename-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.lrc"), "a").unwrap();
        fs::write(root.join("b.lrc"), "b").unwrap();
        let mut run = Run::start(&root).unwrap();
        assert!(run.rename(Path::new("a.lrc"), Path::new("b.lrc")).is_err());
        assert_eq!(fs::read_to_string(root.join("b.lrc")).unwrap(), "b");
        assert!(run.finish().unwrap().is_none());
        fs::remove_dir_all(&root).unwrap();
    }
}
