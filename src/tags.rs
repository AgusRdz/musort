use std::borrow::Cow;
use std::fs::File;
use std::path::Path;

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, TextInformationFrame};
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::prelude::*;
use lofty::TextEncoding;
use serde::{Deserialize, Serialize};

/// Fields to overwrite; `None` leaves the field alone, an empty value removes it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edit {
    pub title: Option<String>,
    pub album: Option<String>,
    pub artists: Option<Vec<String>>,
    pub album_artist: Option<String>,
    pub comment: Option<String>,
}

impl Edit {
    pub fn is_empty(&self) -> bool {
        *self == Edit::default()
    }
}

pub fn write(path: &Path, edit: &Edit) -> Result<(), String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let opts = ParseOptions::new().read_properties(false);
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    match ext.as_str() {
        "mp3" => {
            let mut mp3 = MpegFile::read_from(&mut f, opts).map_err(|e| e.to_string())?;
            drop(f);
            if mp3.id3v2().is_none() {
                mp3.set_id3v2(Id3v2Tag::new());
            }
            let tag = mp3.id3v2_mut().unwrap();
            apply_id3v2(tag, edit);
            // ID3v2.4 is required for null-separated multi-value artists.
            tag.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
        }
        "flac" => {
            let mut flac = FlacFile::read_from(&mut f, opts).map_err(|e| e.to_string())?;
            drop(f);
            if flac.vorbis_comments().is_none() {
                flac.set_vorbis_comments(VorbisComments::new());
            }
            let tag = flac.vorbis_comments_mut().unwrap();
            apply_vorbis(tag, edit);
            tag.save_to_path(path, WriteOptions::default()).map_err(|e| e.to_string())
        }
        _ => Err(format!("formato no soportado para escribir: .{ext}")),
    }
}

fn apply_id3v2(tag: &mut Id3v2Tag, e: &Edit) {
    if let Some(v) = &e.title {
        if v.is_empty() { tag.remove_title() } else { tag.set_title(v.clone()) }
    }
    if let Some(v) = &e.album {
        if v.is_empty() { tag.remove_album() } else { tag.set_album(v.clone()) }
    }
    if let Some(v) = &e.artists {
        tag.set_artist(v.join("\0"));
    }
    if let Some(v) = &e.album_artist {
        let id = FrameId::Valid(Cow::Borrowed("TPE2"));
        tag.remove(&id).for_each(drop);
        if !v.is_empty() {
            tag.insert(Frame::Text(TextInformationFrame::new(id, TextEncoding::UTF8, v.clone())));
        }
    }
    if let Some(v) = &e.comment {
        if v.is_empty() { tag.remove_comment() } else { tag.set_comment(v.clone()) }
    }
}

fn apply_vorbis(tag: &mut VorbisComments, e: &Edit) {
    if let Some(v) = &e.title {
        if v.is_empty() { tag.remove_title() } else { tag.set_title(v.clone()) }
    }
    if let Some(v) = &e.album {
        if v.is_empty() { tag.remove_album() } else { tag.set_album(v.clone()) }
    }
    if let Some(v) = &e.artists {
        tag.remove("ARTIST").for_each(drop);
        for a in v {
            tag.push("ARTIST".into(), a.clone());
        }
    }
    if let Some(v) = &e.album_artist {
        tag.remove("ALBUMARTIST").for_each(drop);
        if !v.is_empty() {
            tag.insert("ALBUMARTIST".into(), v.clone());
        }
    }
    if let Some(v) = &e.comment {
        if v.is_empty() { tag.remove_comment() } else { tag.set_comment(v.clone()) }
    }
}
