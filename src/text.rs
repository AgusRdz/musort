use unicode_normalization::{is_nfc, UnicodeNormalization};

pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

pub fn is_normalized(s: &str) -> bool {
    is_nfc(s)
}

/// Comparison key: lowercase, accents removed, only letters and digits.
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Title without version suffixes: "Monitor - Acoustic" / "Paula (En Vivo)" -> "Monitor" / "Paula".
pub fn base_title(title: &str) -> String {
    let cut = [" - ", " (", " ["]
        .iter()
        .filter_map(|sep| title.find(sep))
        .min()
        .unwrap_or(title.len());
    fold(&title[..cut])
}

/// Artist tags store collaborations as "A, B, C". "/" is left alone because of names like AC/DC.
pub fn primary_artist(artist: &str) -> &str {
    artist.split([',', ';']).next().unwrap_or(artist).trim()
}

pub fn is_junk(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("www.") || s.contains("http") || s.contains(".com") || s.contains("downloaded from")
}

/// Drops " - www.site.com" style segments that download sites append to tags.
pub fn strip_junk(s: &str) -> String {
    s.split(" - ").filter(|part| !is_junk(part)).collect::<Vec<_>>().join(" - ").trim().to_owned()
}

/// Splits "A, B; C" into separate artists. Adjacent pieces are kept together when they form a
/// name in `known` (folded), so a band like "Earth, Wind & Fire" survives.
pub fn split_artists(values: &[String], known: &std::collections::HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let parts: Vec<String> = value
            .split([',', ';'])
            .map(|p| strip_junk(p.trim()))
            .filter(|p| !p.is_empty())
            .collect();
        let mut i = 0;
        while i < parts.len() {
            let end = (i + 1..=parts.len())
                .rev()
                .find(|&j| j == i + 1 || known.contains(&fold(&parts[i..j].join(", "))))
                .unwrap();
            out.push(parts[i..end].join(", "));
            i = end;
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|a| seen.insert(fold(a)));
    out
}

/// Removes ad suffixes ("Title | Band 2016"), an "Artist - " prefix, and repeated "(feat. X)".
pub fn clean_title(title: &str, artist_names: &[&str]) -> String {
    let mut t = strip_junk(title);
    if let Some((before, _)) = t.split_once(" | ") {
        t = before.to_owned();
    }
    if let Some((prefix, rest)) = t.split_once(" - ") {
        if !rest.is_empty() && artist_names.iter().any(|a| fold(a) == fold(prefix)) {
            t = rest.to_owned();
        }
    }
    let mut seen = Vec::new();
    let mut out = String::new();
    let mut rest = t.as_str();
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')').map(|c| open + c + 1) else { break };
        let group = &rest[open..close];
        if seen.contains(&fold(group)) {
            out.push_str(rest[..open].trim_end());
        } else {
            seen.push(fold(group));
            out.push_str(&rest[..close]);
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// Name as it must appear on disk. The microSD is FAT32/exFAT, which rejects these characters
/// and drops trailing dots ("R.E.M." is "R.E.M" on the card), so the library matches it.
pub fn sanitize_filename(s: &str) -> String {
    let replaced: String = s
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' | ':' => Some(' '),
            '*' | '?' | '"' | '<' | '>' | '|' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    nfc(collapsed.trim_end_matches(['.', ' ']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_accents_and_punctuation() {
        assert_eq!(fold("¡Ay Amigo!"), fold("Ay Amigo"));
        assert_eq!(fold("Un Millón de Rosas"), fold("Un Millon de Rosas"));
    }

    #[test]
    fn base_title_strips_versions() {
        assert_eq!(base_title("Monitor - Acoustic"), "monitor");
        assert_eq!(base_title("Amargo Adiós (Version Ska)"), "amargoadios");
    }

    #[test]
    fn primary_artist_keeps_slash_names() {
        assert_eq!(primary_artist("Calibre 50, El Komander"), "Calibre 50");
        assert_eq!(primary_artist("AC/DC"), "AC/DC");
    }

    #[test]
    fn strips_site_junk() {
        assert_eq!(
            strip_junk("Banda Cuisillos De Arturo Macías - www.Mix-Group.jimdo.com"),
            "Banda Cuisillos De Arturo Macías"
        );
        assert_eq!(strip_junk("Carpe Diem - Demo"), "Carpe Diem - Demo");
    }

    #[test]
    fn splits_artists_but_keeps_known_comma_names() {
        let known = ["earthwindfire".to_owned()].into_iter().collect();
        let v = |s: &str| vec![s.to_owned()];
        assert_eq!(split_artists(&v("Calibre 50, El Komander"), &known), ["Calibre 50", "El Komander"]);
        assert_eq!(split_artists(&v("Earth, Wind & Fire, Calibre 50"), &known), ["Earth, Wind & Fire", "Calibre 50"]);
        assert_eq!(split_artists(&v("AC/DC"), &known), ["AC/DC"]);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Mujer Paseada | La Trakalosa De Monterrey 2016", &[]), "Mujer Paseada");
        assert_eq!(
            clean_title("Edwin Luna y La Trakalosa De Monterrey - Dos Monedas", &["Edwin Luna y La Trakalosa de Monterrey"]),
            "Dos Monedas"
        );
        assert_eq!(
            clean_title("Beso Al Aire (feat. América Sierra & Smoky) (feat. América Sierra & Smoky)", &[]),
            "Beso Al Aire (feat. América Sierra & Smoky)"
        );
        assert_eq!(clean_title("Carpe Diem - Demo", &["Green Day"]), "Carpe Diem - Demo");
    }

    #[test]
    fn sanitizes_like_existing_library() {
        assert_eq!(sanitize_filename("AC/DC"), "AC DC");
        assert_eq!(sanitize_filename("Arnulfo Jr. \"Rey Y As\""), "Arnulfo Jr. Rey Y As");
        assert_eq!(sanitize_filename("Yusuf / Cat Stevens"), "Yusuf Cat Stevens");
    }
}
