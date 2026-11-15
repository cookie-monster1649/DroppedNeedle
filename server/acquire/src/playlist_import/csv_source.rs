//! Fork: the playlist CSV source. Spotify will not let a development-mode
//! app read another user's playlists, so the playlists can be exported
//! instead (Exportify, exportify.net: one `.csv` per playlist) into a
//! folder named by `PLAYLIST_IMPORT_PLAYLIST_CSVS`. When it is set it
//! takes the place of the Spotify API in the public-playlist match: each
//! file is one playlist, named by its file name, and its rows are the
//! entries matched against the leftovers exactly as the Spotify entries
//! are ([`super::spotify_match`]).
//!
//! - Columns are read by header name, in any order; unknown columns are
//!   ignored. A byte-order mark, quoted fields, doubled quotes, commas and
//!   line breaks inside quotes all read.
//! - Exportify separates artists with `;` (older exports used `,`; both
//!   read).
//! - The folder is read again on every run: nothing is called, so nothing
//!   is cached beyond the state's catalogue.

use std::collections::BTreeMap;
use std::path::Path;

use super::spotify_match::{Cached, Catalogue, Item};

/// The folder of playlist CSV files.
pub const CSVS_ENV: &str = "PLAYLIST_IMPORT_PLAYLIST_CSVS";
/// The prefix of a CSV playlist's id in the catalogue (and of the
/// catalogue's source key).
pub const ID_PREFIX: &str = "csv:";

/// The folder the environment names, if any.
pub fn dir_from_env() -> Option<std::path::PathBuf> {
    std::env::var_os(CSVS_ENV)
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// The folder the playlist CSVs are read from: the one the environment
/// names, else the import source itself when it holds top-level `.csv`
/// files (the audio walk only takes subfolders, so they never import).
pub fn dir_for(source: Option<&Path>) -> Option<std::path::PathBuf> {
    dir_from_env().or_else(|| source.filter(|dir| has_csvs(dir)).map(Path::to_path_buf))
}

/// Whether `dir` holds a top-level `.csv` file.
pub fn has_csvs(dir: &Path) -> bool {
    csv_paths(dir).is_ok_and(|paths| !paths.is_empty())
}

/// The visible `.csv` files directly in `dir`, sorted.
fn csv_paths(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    paths.sort();
    Ok(paths)
}

/// The records of a CSV text (RFC 4180, leniently): a leading BOM is
/// dropped, fields may be quoted, a doubled quote inside quotes is one
/// quote, and CRLF or LF ends a record. Blank lines are skipped.
pub fn records(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let end_record = |record: &mut Vec<String>, field: &mut String, out: &mut Vec<Vec<String>>| {
        record.push(std::mem::take(field));
        let taken = std::mem::take(record);
        if !(taken.len() == 1 && taken[0].trim().is_empty()) {
            out.push(taken);
        }
    };
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                end_record(&mut record, &mut field, &mut out);
            }
            '\n' => end_record(&mut record, &mut field, &mut out),
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        end_record(&mut record, &mut field, &mut out);
    }
    out
}

/// A header name folded for lookup: case and spacing ignored.
fn header_key(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The entries of one Exportify-style CSV. Needs at least a track name
/// and an artist column.
///
/// # Errors
///
/// The text has no header, or the header names no track or artist.
pub fn items(text: &str) -> Result<Vec<Item>, String> {
    let mut rows = records(text).into_iter();
    let header = rows.next().ok_or("the file is empty")?;
    let column = |names: &[&str]| {
        header.iter().position(|name| {
            names
                .iter()
                .any(|want| header_key(name) == header_key(want))
        })
    };
    let title = column(&["Track Name", "Name", "Title"]).ok_or("no \"Track Name\" column")?;
    let artist = column(&["Artist Name(s)", "Artist Names", "Artist Name", "Artist"])
        .ok_or("no \"Artist Name(s)\" column")?;
    let uri = column(&["Track URI", "Spotify URI", "URI"]);
    let album = column(&["Album Name", "Album"]);
    let duration = column(&["Duration (ms)", "Track Duration (ms)", "Duration"]);
    let isrc = column(&["ISRC"]);
    let cell = |row: &[String], at: Option<usize>| -> String {
        at.and_then(|at| row.get(at))
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for row in rows {
        let name = cell(&row, Some(title));
        if name.is_empty() {
            continue;
        }
        let credit = cell(&row, Some(artist));
        let artists: Vec<String> = if credit.contains(';') {
            credit.split(';')
        } else {
            credit.split(',')
        }
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
        let id = cell(&row, uri);
        let id = id
            .strip_prefix("spotify:track:")
            .or_else(|| {
                id.split("/track/")
                    .nth(1)
                    .map(|rest| rest.split(['/', '?', '#']).next().unwrap_or(rest))
            })
            .unwrap_or(&id)
            .to_owned();
        let seconds = cell(&row, duration)
            .parse::<f64>()
            .ok()
            .filter(|ms| ms.is_finite() && *ms > 0.0)
            .map(|ms| ms / 1000.0);
        let isrc = Some(cell(&row, isrc).replace('-', "").to_ascii_uppercase())
            .filter(|code| code.len() == 12 && code.chars().all(|c| c.is_ascii_alphanumeric()));
        out.push(Item {
            // A local file has no Spotify id: the row's place stands in.
            id: if id.is_empty() {
                format!("row{}", out.len() + 1)
            } else {
                id
            },
            title: name,
            artists,
            album: cell(&row, album),
            album_id: None,
            seconds,
            isrc,
        });
    }
    Ok(out)
}

/// What one read of the folder found: the playlists by id, and the files
/// that did not read.
#[derive(Debug, Default)]
pub struct Read {
    pub playlists: BTreeMap<String, Cached>,
    pub failed: Vec<String>,
}

/// Read every `.csv` directly in `dir`. Blocking.
///
/// # Errors
///
/// The folder does not list.
pub fn read_dir(dir: &Path) -> Result<Read, String> {
    let paths = csv_paths(dir)
        .map_err(|error| format!("the playlist CSV folder does not list: {error}"))?;
    let mut read = Read::default();
    for path in paths {
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let parsed = std::fs::read(&path)
            .map_err(|error| error.to_string())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .and_then(|text| items(&text));
        match parsed {
            Ok(items) => {
                read.playlists.insert(
                    format!("{ID_PREFIX}{name}"),
                    Cached {
                        name,
                        items: Some(items),
                        ..Cached::default()
                    },
                );
            }
            Err(error) => read.failed.push(format!("{name}: {error}")),
        }
    }
    Ok(read)
}

/// Whether a catalogue was read from playlist CSV files.
pub fn is_csv(catalogue: &Catalogue) -> bool {
    catalogue.source.starts_with(ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header Exportify writes (pavelkomarov/exportify, exportify.js),
    /// with its BOM, and rows as it quotes them.
    const EXPORTIFY: &str = "\u{feff}Track URI,ISRC,Track Name,Album Name,Artist Name(s),Release Date,Duration (ms),Popularity,Explicit,Added By,Added At,Genres,Record Label,Danceability,Energy,Key,Loudness,Mode,Speechiness,Acousticness,Instrumentalness,Liveness,Valence,Tempo,Time Signature\n\
spotify:track:4uLU6hMCjMI75M1A2tKUQC,USUM70905526,\"Strobe - Radio Edit\",\"For Lack Of A Better Name\",\"deadmau5\",2009-09-22,214000,61,false,someone,2020-01-01T00:00:00Z,\"progressive house,electro house\",\"mau5trap\",0.5,0.6,1,-8.1,1,0.04,0.01,0.8,0.1,0.3,128.0,4\n\
spotify:track:0abc,GB-ABC-24-00001,\"Say \"\"Hello\"\", Again\",\"Album, With Comma\",\"Above & Beyond;Zoë Johnston\",2004,401500,40,false,someone,2020-01-01T00:00:00Z,\"\",\"\",,,,,,,,,,,\n\
\n\
spotify:local:Artist:Album:Local+Song:200,,\"Local Song\",\"Album\",\"Someone\",,200000,0,false,someone,2020-01-01T00:00:00Z,\"\",\"\",,,,,,,,,,,\r\n";

    #[test]
    fn an_exportify_csv_reads_by_header_name() {
        let items = items(EXPORTIFY).unwrap();
        assert_eq!(items.len(), 3);
        let strobe = &items[0];
        assert_eq!(strobe.id, "4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(strobe.title, "Strobe - Radio Edit");
        assert_eq!(strobe.album, "For Lack Of A Better Name");
        assert_eq!(strobe.artists, vec!["deadmau5".to_owned()]);
        assert_eq!(strobe.seconds, Some(214.0));
        assert_eq!(strobe.isrc.as_deref(), Some("USUM70905526"));
        assert_eq!(strobe.album_id, None);
        let hello = &items[1];
        assert_eq!(hello.title, "Say \"Hello\", Again");
        assert_eq!(hello.album, "Album, With Comma");
        assert_eq!(
            hello.artists,
            vec!["Above & Beyond".to_owned(), "Zoë Johnston".to_owned()]
        );
        assert_eq!(hello.isrc.as_deref(), Some("GBABC2400001"));
        assert_eq!(hello.seconds, Some(401.5));
        // A local file: no ISRC, its URI kept as the id.
        assert_eq!(items[2].isrc, None);
        assert_eq!(items[2].title, "Local Song");
    }

    #[test]
    fn columns_in_another_order_and_older_comma_artists_read() {
        let text = "Artist Name(s),Track Name,Duration (ms)\n\"A, B\",Song,1000\n";
        let items = items(text).unwrap();
        assert_eq!(items[0].artists, vec!["A".to_owned(), "B".to_owned()]);
        assert_eq!(items[0].title, "Song");
        assert_eq!(items[0].seconds, Some(1.0));
        assert_eq!(items[0].isrc, None);
        assert_eq!(items[0].id, "row1");
        assert!(super::items("Album Name\nX\n").is_err());
        assert!(super::items("").is_err());
    }

    #[test]
    fn quoted_line_breaks_and_crlf_read() {
        let rows = records("a,\"b\r\nc\",d\r\n1,2,3");
        assert_eq!(
            rows,
            vec![
                vec!["a".to_owned(), "b\r\nc".to_owned(), "d".to_owned()],
                vec!["1".to_owned(), "2".to_owned(), "3".to_owned()],
            ]
        );
    }

    #[test]
    fn a_folder_reads_one_playlist_per_file_named_by_the_file() {
        let dir = std::env::temp_dir().join(format!("dn-pi-csv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("6PM_Sunset.csv"), EXPORTIFY).unwrap();
        std::fs::write(dir.join("broken.csv"), "Album Name\nX\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let read = read_dir(&dir).unwrap();
        assert_eq!(read.playlists.len(), 1);
        let cached = &read.playlists["csv:6PM_Sunset"];
        assert_eq!(cached.name, "6PM_Sunset");
        assert_eq!(cached.items.as_ref().map(Vec::len), Some(3));
        assert_eq!(read.failed.len(), 1);
        // Exportify's underscores fold away: the file maps to its folder.
        assert_eq!(
            super::super::spotify_match::fold_name("6PM_Sunset"),
            super::super::spotify_match::fold_name("6PM Sunset")
        );
        assert!(has_csvs(&dir));
        // The audio walk never takes the top-level CSVs.
        assert!(super::super::walk::walk(&dir).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!has_csvs(&dir));
    }
}
