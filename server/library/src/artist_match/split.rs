//! Fork: joined artist credits split into their artists.
//!
//! Tags from playlist folders and shops often hold several artists in one
//! string ("Jamie xx/Robyn", "Mogli, Kilian & Jo, Henry Green"). Such a
//! string is split here, against what the library knows:
//!
//! - a string that is itself a known artist (a live artist with a
//!   MusicBrainz link, whose folded name is the string's) is never split;
//! - `/` and `;` always split the rest, except where a part is too short
//!   to be a name on its own ("AC/DC");
//! - `, `, ` & `, ` x `, ` vs ` and `feat.`/`ft.` split only when every
//!   part is a known artist. Neighbouring parts that are one known artist
//!   together stay together ("Kilian & Jo"), and a string whose parts are
//!   not all known stays whole ("Simon & Garfunkel", "Earth, Wind & Fire").
//!
//! The same rule runs when a file is indexed (`scan::sqlite_store::artists`)
//! and in the one-time backfill ([`super::credits`]), so a file indexed
//! again lands where the backfill put its credits.

use droppedneedle_kernel::db::fold_text;

/// The fewest letters or digits a part split off by `/` or `;` has.
const MIN_HARD_PART: usize = 3;

/// One artist of a split credit and the text joining it to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditPart {
    pub name: String,
    pub join_phrase: String,
}

/// Whether `folded` (as [`fold_text`] folds) is a known artist's name.
pub trait KnownArtists {
    fn known(&self, folded: &str) -> bool;
}

impl<F: Fn(&str) -> bool> KnownArtists for F {
    fn known(&self, folded: &str) -> bool {
        self(folded)
    }
}

/// Whether `name` is a known artist, as written or with its `/` and `;`
/// read as " & " or " and " (MusicBrainz writes "Damian Lazarus & The
/// Ancient Moons" where a tag writes "Damian Lazarus/The Ancient Moons").
fn known_name(name: &str, known: &impl KnownArtists) -> bool {
    let folded = fold_text(name);
    if folded.is_empty() {
        return false;
    }
    if known.known(&folded) {
        return true;
    }
    if !folded.contains(['/', ';']) {
        return false;
    }
    [" & ", " and "].iter().any(|join| {
        let joined: Vec<&str> = folded
            .split(['/', ';'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        known.known(&joined.join(join))
    })
}

fn letters(part: &str) -> usize {
    part.chars().filter(|c| c.is_alphanumeric()).count()
}

/// Split `text` at every separator `soft_separators` finds, keeping each
/// separator.
fn soft_pieces(text: &str) -> (Vec<String>, Vec<String>) {
    let lower = text.to_lowercase();
    // Positions of separators in byte offsets of `text`; lowercase keeps
    // ASCII offsets, and only ASCII separators are looked for, so offsets
    // are read off a lowercase copy only when its length agrees.
    let hay = if lower.len() == text.len() {
        lower
    } else {
        text.to_owned()
    };
    let seps = [
        ", ",
        " & ",
        " x ",
        " vs. ",
        " vs ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
    ];
    let mut pieces = Vec::new();
    let mut joins = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < hay.len() {
        if !hay.is_char_boundary(at) || !text.is_char_boundary(at) {
            at += 1;
            continue;
        }
        if let Some(sep) = seps.iter().find(|sep| hay[at..].starts_with(*sep))
            && at > start
        {
            pieces.push(text[start..at].to_owned());
            joins.push(text[at..at + sep.len()].to_owned());
            start = at + sep.len();
            at = start;
            continue;
        }
        at += 1;
    }
    pieces.push(text[start..].to_owned());
    (pieces, joins)
}

/// A part split off by `/` or `;`, split again at the soft separators
/// when every resulting part is known. Fewest parts first, so known
/// neighbours stay together.
fn soft_split(part: &str, known: &impl KnownArtists) -> Vec<CreditPart> {
    let whole = || {
        vec![CreditPart {
            name: part.to_owned(),
            join_phrase: String::new(),
        }]
    };
    if known_name(part, known) {
        return whole();
    }
    let (pieces, joins) = soft_pieces(part);
    let n = pieces.len();
    if !(2..=12).contains(&n) {
        return whole();
    }
    // best[j]: the fewest known segments covering pieces[..j].
    let mut best: Vec<Option<Vec<(usize, usize)>>> = vec![None; n + 1];
    best[0] = Some(Vec::new());
    for i in 0..n {
        let Some(so_far) = best[i].clone() else {
            continue;
        };
        #[allow(clippy::needless_range_loop)]
        for j in (i + 1)..=n {
            if i == 0 && j == n {
                continue;
            }
            let mut text = pieces[i].clone();
            for t in i..j - 1 {
                text.push_str(&joins[t]);
                text.push_str(&pieces[t + 1]);
            }
            if !known_name(text.trim(), known) {
                continue;
            }
            let better = best[j]
                .as_ref()
                .is_none_or(|current| current.len() > so_far.len() + 1);
            if better {
                let mut next = so_far.clone();
                next.push((i, j));
                best[j] = Some(next);
            }
        }
    }
    let Some(segments) = best[n].take() else {
        return whole();
    };
    segments
        .iter()
        .map(|&(i, j)| {
            let mut name = pieces[i].clone();
            for t in i..j - 1 {
                name.push_str(&joins[t]);
                name.push_str(&pieces[t + 1]);
            }
            CreditPart {
                name: name.trim().to_owned(),
                join_phrase: joins.get(j - 1).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

/// The artists of credit `name`: one part when it is one artist (or is
/// not to be split), else each artist with the text joining it to the
/// next. A name never yields the same artist twice.
pub fn split_credit(name: &str, known: &impl KnownArtists) -> Vec<CreditPart> {
    let name = name.trim();
    let whole = vec![CreditPart {
        name: name.to_owned(),
        join_phrase: String::new(),
    }];
    if name.is_empty() || known_name(name, known) {
        return whole;
    }
    let mut hard: Vec<(String, String)> = Vec::new();
    let mut rest = name;
    while let Some(at) = rest.find(['/', ';']) {
        let sep = &rest[at..at + 1];
        hard.push((rest[..at].trim().to_owned(), format!(" {sep} ")));
        rest = &rest[at + 1..];
    }
    hard.push((rest.trim().to_owned(), String::new()));
    hard.retain(|(part, _)| !part.is_empty());
    if hard.len() > 1
        && hard
            .iter()
            .any(|(part, _)| letters(part) < MIN_HARD_PART && !known_name(part, known))
    {
        return whole;
    }
    let mut out: Vec<CreditPart> = Vec::new();
    let count = hard.len();
    for (index, (part, join)) in hard.into_iter().enumerate() {
        let mut parts = soft_split(&part, known);
        if index + 1 < count
            && let Some(last) = parts.last_mut()
        {
            last.join_phrase = join.clone();
        }
        out.extend(parts);
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|part| seen.insert(fold_text(&part.name)));
    if let Some(last) = out.last_mut() {
        last.join_phrase.clear();
    }
    if out.len() < 2 { whole } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parts: &[CreditPart]) -> Vec<&str> {
        parts.iter().map(|part| part.name.as_str()).collect()
    }

    fn library(known: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |folded: &str| known.iter().any(|name| fold_text(name) == folded)
    }

    #[test]
    fn slash_lists_split() {
        let known = library(&[]);
        assert_eq!(
            names(&split_credit("Honey Dijon/Tim K/John Mendelsohn", &known)),
            ["Honey Dijon", "Tim K", "John Mendelsohn"]
        );
        let parts = split_credit("Jamie xx/Robyn", &known);
        assert_eq!(names(&parts), ["Jamie xx", "Robyn"]);
        assert_eq!(parts[0].join_phrase, " / ");
        assert_eq!(parts[1].join_phrase, "");
        assert_eq!(
            names(&split_credit("A Band; B Band", &known)),
            ["A Band", "B Band"]
        );
    }

    #[test]
    fn duos_stay_whole() {
        let known = library(&["Mogli", "Henry Green", "Kilian & Jo"]);
        for name in [
            "Kilian & Jo",
            "Simon & Garfunkel",
            "Earth, Wind & Fire",
            "AC/DC",
        ] {
            assert_eq!(names(&split_credit(name, &known)), [name], "{name}");
        }
        assert_eq!(
            names(&split_credit("Mogli, Kilian & Jo, Henry Green", &known)),
            ["Mogli", "Kilian & Jo", "Henry Green"]
        );
        // One part unknown: the comma list stays whole.
        assert_eq!(
            names(&split_credit("Mogli, Somebody New", &known)),
            ["Mogli, Somebody New"]
        );
    }

    #[test]
    fn a_known_whole_is_never_split() {
        let known = library(&[
            "Damian Lazarus & The Ancient Moons",
            "Dubfire/Oliver Huntemann",
        ]);
        for name in [
            "Damian Lazarus/The Ancient Moons",
            "Dubfire/Oliver Huntemann",
        ] {
            assert_eq!(names(&split_credit(name, &known)), [name]);
        }
    }

    #[test]
    fn soft_parts_inside_a_slash_list() {
        let known = library(&["Anyma", "Chris Avantgarde"]);
        assert_eq!(
            names(&split_credit("Anyma x Chris Avantgarde/Someone", &known)),
            ["Anyma", "Chris Avantgarde", "Someone"]
        );
    }

    #[test]
    fn splitting_is_idempotent() {
        let known = library(&["Mogli", "Henry Green"]);
        for name in ["Jamie xx/Robyn", "Mogli, Henry Green", "Robyn"] {
            for part in split_credit(name, &known) {
                assert_eq!(
                    names(&split_credit(&part.name, &known)),
                    [part.name.as_str()]
                );
            }
        }
        assert_eq!(names(&split_credit("Robyn/Robyn", &known)), ["Robyn/Robyn"]);
    }
}
