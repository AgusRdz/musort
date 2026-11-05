//! Decisions that must survive between runs, stored in the library so they follow it.
//! Hidden, so Jellyfin ignores it, and not music, so `sync` doesn't copy it.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::text::fold;

pub const FILE: &str = ".musort.json";

#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    /// Folder pairs (folded, sorted) the user said are different artists.
    #[serde(default)]
    pub distinct: Vec<(String, String)>,
}

fn pair(a: &str, b: &str) -> (String, String) {
    let (a, b) = (fold(a), fold(b));
    if a <= b { (a, b) } else { (b, a) }
}

impl Config {
    pub fn load(root: &Path) -> Config {
        fs::read(root.join(FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, root: &Path) -> io::Result<()> {
        fs::write(root.join(FILE), serde_json::to_vec_pretty(self)?)
    }

    pub fn is_distinct(&self, a: &str, b: &str) -> bool {
        self.distinct.contains(&pair(a, b))
    }

    pub fn mark_distinct(&mut self, a: &str, b: &str) {
        if !self.is_distinct(a, b) {
            self.distinct.push(pair(a, b));
        }
    }
}
