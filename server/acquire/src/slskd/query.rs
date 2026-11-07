//! Soulseek query construction: sanitizing, artist wildcards, ladders.
//!
//! Soulseek ANDs every query word, so a specific query sometimes returns
//! nothing when a broader one returns thousands (verified live), so the
//! ladders escalate from most-specific to broadest, and the scorer narrows
//! back down.

/// Strip Soulseek operators (space-surrounded hyphens, parentheses) that
/// confuse the search, while preserving hyphens inside names like `AC-DC` /
/// `Jay-Z`. Typographic apostrophes are normalised
/// to the straight ASCII form: MusicBrainz metadata uses ' but shared files
/// are almost always named with `'`.
#[must_use]
pub fn sanitize_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for ch in query.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{02BC}' => out.push('\''),
            '(' | ')' => out.push(' '),
            _ => out.push(ch),
        }
    }
    // Collapse space-surrounded hyphens without touching in-word hyphens.
    let mut collapsed = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let is_spaced_hyphen = chars[index] == '-'
            && index > 0
            && chars[index - 1].is_whitespace()
            && index + 1 < chars.len()
            && chars[index + 1].is_whitespace();
        if is_spaced_hyphen {
            collapsed.push(' ');
            index += 1;
        } else {
            collapsed.push(chars[index]);
            index += 1;
        }
    }
    collapsed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First credited artist of a joined credit string.
/// MusicBrainz joins multi-artist credits (`"YMO, ..."`) but Soulseek ANDs
/// every query word, so each extra credit is another term no peer path
/// contains (issue #373). Query rungs use only the primary artist.
#[must_use]
pub fn primary_artist(artist: &str) -> &str {
    let first = artist.split(',').next().unwrap_or(artist).trim();
    if first.is_empty() { artist } else { first }
}

/// Album title without edition parentheticals or a subtitle tail.
/// `Euphoria (International Edition)` -> `Euphoria`;
/// `Devil May Cry: Season 2 (Soundtrack ...)` -> `Devil May Cry`.
/// Last-resort rungs only: the scorer still narrows the broader result set
/// back down.
#[must_use]
pub fn stripped_album_title(album: &str) -> String {
    let no_parens = strip_balanced(album, '(', ')');
    let no_brackets = strip_balanced(&no_parens, '[', ']');
    let no_subtitle = no_brackets.split(':').next().unwrap_or("").to_owned();
    no_subtitle.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_balanced(text: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for ch in text.chars() {
        if ch == open {
            depth += 1;
            out.push(' ');
        } else if ch == close {
            if depth > 0 {
                depth -= 1;
            }
            out.push(' ');
        } else if depth == 0 {
            out.push(ch);
        } else {
            out.push(' ');
        }
    }
    out
}

/// Blocked-artist workaround: Soulseek's server
/// filters searches containing certain artist terms (DMCA), returning 0
/// results no matter what else is in the query ("Enter Shikari" ->
/// nothing). Replacing the first letter of each word with Soulseek's
/// leading wildcard defeats the filter while matching the same files
/// ("*nter *hikari" -> lots). An apostrophe right after the first letter is
/// absorbed into the wildcard (D'Angelo -> *Angelo) so matching no longer
/// depends on the peer's apostrophe form.
#[must_use]
pub fn wildcard_artist(artist: &str) -> String {
    artist
        .split_whitespace()
        .map(wildcard_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn wildcard_word(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if !first.is_alphabetic() {
        return word.to_owned();
    }
    let mut rest: String = chars.collect();
    if rest.starts_with('\'') || rest.starts_with('\u{2019}') {
        rest = rest.chars().skip(1).collect();
    }
    // Keep short words exact: "*" plus a lone char matches far too much.
    if rest.chars().count() >= 2 {
        format!("*{rest}")
    } else {
        word.to_owned()
    }
}

fn dedupe_queries(queries: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for query in queries {
        if !query.is_empty() && seen.insert(query.clone()) {
            out.push(query);
        }
    }
    out
}

fn build_album_query(artist: &str, album: &str, year: Option<i32>) -> String {
    let mut parts = vec![artist.to_owned(), album.to_owned()];
    if let Some(year) = year {
        parts.push(year.to_string());
    }
    sanitize_query(&parts.join(" - "))
}

fn build_track_query(artist: &str, track: &str, album: Option<&str>) -> String {
    let mut parts = vec![artist.to_owned(), track.to_owned()];
    if let Some(album) = album {
        parts.push(album.to_owned());
    }
    sanitize_query(&parts.join(" - "))
}

/// Most-specific-first album queries:
/// artist+album+year -> artist+album -> edition-stripped title rungs ->
/// artist. Every rung queries the primary credited artist only. Each rung
/// is followed by a blocked-artist variant with the artist's first letters
/// wildcarded: exact goes first because wildcards degrade matching on some
/// clients; the wildcard sibling comes before broadening so a blocked artist
/// still gets the most specific query that can return anything.
#[must_use]
pub fn album_query_ladder(artist: &str, album: &str, year: Option<i32>) -> Vec<String> {
    let primary = primary_artist(artist);
    let wildcard = wildcard_artist(&sanitize_query(primary));
    let mut queries = vec![
        build_album_query(primary, album, year),
        build_album_query(&wildcard, album, year),
        build_album_query(primary, album, None),
        build_album_query(&wildcard, album, None),
    ];
    let stripped = stripped_album_title(album);
    if !stripped.is_empty() && stripped != album.split_whitespace().collect::<Vec<_>>().join(" ") {
        queries.extend([
            build_album_query(primary, &stripped, year),
            build_album_query(&wildcard, &stripped, year),
            build_album_query(primary, &stripped, None),
            build_album_query(&wildcard, &stripped, None),
        ]);
    }
    queries.extend([sanitize_query(primary), wildcard]);
    dedupe_queries(queries)
}

/// Most-specific-first track queries:
/// artist+track+album -> artist+track -> artist+core title (featured
/// credits and noise brackets dropped, a distinctive version word kept) ->
/// each co-credited artist+core title. Keeps the track title at every rung
/// so the track matcher can match, and never adds a year. Wildcard
/// blocked-artist variants interleave as in [`album_query_ladder`].
#[must_use]
pub fn track_query_ladder(artist: &str, track: &str, album: Option<&str>) -> Vec<String> {
    let primary = primary_artist(artist);
    let wildcard = wildcard_artist(&sanitize_query(primary));
    let mut queries = vec![
        build_track_query(primary, track, album),
        build_track_query(&wildcard, track, album),
        build_track_query(primary, track, None),
        build_track_query(&wildcard, track, None),
    ];
    let core = core_track_title(track);
    if !core.is_empty() {
        queries.push(build_track_query(primary, &core, None));
        queries.push(build_track_query(&wildcard, &core, None));
        // A collaboration is often filed under the other artist.
        for other in co_credited_artists(artist, track) {
            queries.push(build_track_query(&other, &core, None));
        }
    }
    dedupe_queries(queries)
}

/// Words that name a different recording than the plain track, worth
/// keeping in a query: without them the plain version crowds the results.
/// Remaster and edition words are left out on purpose: they rarely appear
/// in shared file names and narrow a search to nothing.
const VERSION_WORDS: [&str; 12] = [
    "live",
    "remix",
    "vip",
    "acoustic",
    "instrumental",
    "demo",
    "extended",
    "dub",
    "reprise",
    "unplugged",
    "edit",
    "mix",
];

const FEATURE_MARKERS: [&str; 5] = ["feat.", "feat", "ft.", "ft", "featuring"];

/// The track title for a query: featured credits and bracketed noise
/// removed, a bracket naming a version kept as plain words.
/// `Song (feat. X) [Live]` -> `Song Live`;
/// `Song (Skrillex Remix)` -> `Song Skrillex Remix`;
/// `Song - 2011 Remaster` -> `Song`.
#[must_use]
pub fn core_track_title(track: &str) -> String {
    let mut base = String::new();
    let mut kept: Vec<String> = Vec::new();
    let mut depth = 0usize;
    let mut inner = String::new();
    for ch in track.chars() {
        match ch {
            '(' | '[' => {
                if depth == 0 {
                    inner.clear();
                } else {
                    inner.push(ch);
                }
                depth += 1;
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(version) = version_phrase(&inner) {
                        kept.push(version);
                    }
                } else {
                    inner.push(ch);
                }
            }
            _ if depth > 0 => inner.push(ch),
            _ => base.push(ch),
        }
    }
    // A dashed tail ("Song - Live at X", "Song - 2011 Remaster").
    let mut base = match base.split_once(" - ") {
        Some((head, tail)) => {
            if let Some(version) = version_phrase(tail) {
                kept.push(version);
            }
            head.to_owned()
        }
        None => base,
    };
    // An unbracketed "feat." tail.
    let lowered = base.to_lowercase();
    if let Some(at) = lowered
        .split_whitespace()
        .position(|word| FEATURE_MARKERS.contains(&word))
    {
        base = base
            .split_whitespace()
            .take(at)
            .collect::<Vec<_>>()
            .join(" ");
    }
    let mut words: Vec<String> = base.split_whitespace().map(str::to_owned).collect();
    words.extend(kept);
    words.join(" ")
}

/// A bracket or dashed tail as query words when it names a version, else
/// `None`. Featured credits are never a version.
fn version_phrase(text: &str) -> Option<String> {
    let lowered = text.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let first = words.first()?;
    if FEATURE_MARKERS.contains(first) || *first == "with" {
        return None;
    }
    if !words.iter().any(|word| VERSION_WORDS.contains(word)) {
        return None;
    }
    // "Live at Wembley 1986" -> "Live": a venue or year narrows to nothing.
    if *first == "live" {
        return Some("Live".to_owned());
    }
    Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Artists credited beside the primary one, from the joined credit and from
/// a featured credit in the title, in credit order, without repeats.
/// `Calvin Harris, Rihanna` -> `Rihanna`; `A feat. B & C` -> `B`, `C`;
/// title `Song (feat. D)` -> `D`. The primary credit itself is never split:
/// "Simon & Garfunkel" stays one artist.
#[must_use]
pub fn co_credited_artists(artist: &str, track: &str) -> Vec<String> {
    let primary = primary_artist(artist);
    let mut names: Vec<String> = Vec::new();
    let mut push_all = |text: &str| {
        for name in split_credit(text) {
            if !name.eq_ignore_ascii_case(primary)
                && !names.iter().any(|seen| seen.eq_ignore_ascii_case(&name))
            {
                names.push(name);
            }
        }
    };
    // Credits after the first comma.
    if let Some((_, rest)) = artist.split_once(',') {
        push_all(rest);
    }
    // A "feat." inside the artist credit or the title.
    for text in [artist, track] {
        if let Some(featured) = featured_part(text) {
            push_all(&featured);
        }
    }
    names
}

fn featured_part(text: &str) -> Option<String> {
    let cleaned: String = text
        .chars()
        .map(|ch| {
            if matches!(ch, '(' | ')' | '[' | ']') {
                ' '
            } else {
                ch
            }
        })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let at = words
        .iter()
        .position(|word| FEATURE_MARKERS.contains(&word.to_lowercase().as_str()))?;
    let tail: Vec<&str> = words[at + 1..]
        .iter()
        .copied()
        .take_while(|word| !word.starts_with('-'))
        .collect();
    (!tail.is_empty()).then(|| tail.join(" "))
}

fn split_credit(text: &str) -> Vec<String> {
    let mut parts = vec![text.to_owned()];
    for separator in [",", " & ", " x ", " X ", " and ", " with "] {
        parts = parts
            .iter()
            .flat_map(|part| part.split(separator).map(str::to_owned).collect::<Vec<_>>())
            .collect();
    }
    parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

#[cfg(test)]
mod ladder_tests {
    use super::*;

    #[test]
    fn core_title_drops_features_and_noise_but_keeps_versions() {
        assert_eq!(core_track_title("Song (feat. X) [Live]"), "Song Live");
        assert_eq!(
            core_track_title("Song (Skrillex Remix)"),
            "Song Skrillex Remix"
        );
        assert_eq!(core_track_title("Song - 2011 Remaster"), "Song");
        assert_eq!(core_track_title("Song - Live at Wembley 1986"), "Song Live");
        assert_eq!(core_track_title("Song feat. Somebody"), "Song");
        assert_eq!(core_track_title("Song (Deluxe)"), "Song");
        assert_eq!(core_track_title("Plain Song"), "Plain Song");
        assert_eq!(core_track_title("Song (Radio Edit)"), "Song Radio Edit");
    }

    #[test]
    fn co_credits_come_from_credit_and_title_but_never_split_the_primary() {
        assert_eq!(
            co_credited_artists("Calvin Harris, Rihanna", "This Is What"),
            ["Rihanna"]
        );
        assert_eq!(co_credited_artists("A feat. B & C", "Song"), ["B", "C"]);
        assert_eq!(
            co_credited_artists("Daft Punk", "Get Lucky (feat. Pharrell Williams)"),
            ["Pharrell Williams"]
        );
        assert!(co_credited_artists("Simon & Garfunkel", "The Boxer").is_empty());
    }

    #[test]
    fn track_ladder_escalates_without_a_year() {
        let ladder = track_query_ladder(
            "Daft Punk",
            "Get Lucky (feat. Pharrell Williams)",
            Some("Random Access Memories"),
        );
        assert_eq!(
            ladder[0],
            "Daft Punk Get Lucky feat. Pharrell Williams Random Access Memories"
        );
        assert!(ladder.contains(&"Daft Punk Get Lucky".to_owned()));
        assert!(ladder.contains(&"Pharrell Williams Get Lucky".to_owned()));
        assert!(
            ladder
                .iter()
                .all(|query| !query.chars().any(|ch| ch.is_ascii_digit()))
        );
        // A plain title adds no rungs beyond v3's four.
        assert_eq!(track_query_ladder("Low", "Plain", None).len(), 2);
    }
}
