//! Extension chains: schema this build adds beside the upstream chain.
//!
//! The upstream chain ([`super::CHAIN`]) is stamped by `PRAGMA user_version`
//! alone, so a migration that is not upstream's cannot join it: it would
//! take a number upstream later gives its own file, and that file would
//! never run. An extension chain keeps its own count instead, one row per
//! chain in `schema_extensions`, and runs after the upstream chain.
//!
//! The rules that keep the two from ever meeting:
//! - an extension file only creates its own tables and indexes (named for
//!   the area that owns them, as every table is in `table-owners.txt`); it
//!   never alters, rebuilds or drops a table upstream's chain made, and it
//!   never touches `user_version`;
//! - a file on a released chain is never edited, renumbered or removed;
//! - each file runs in one transaction with its own count, so a crash
//!   leaves the file and its count both or neither;
//! - a count past the newest file this binary carries refuses to serve, as
//!   a stamp past the upstream chain does.
//!
//! A database moved back to an upstream build keeps the extension tables;
//! upstream never reads them. When upstream adopts what a chain adds, the
//! chain stops growing and a later file there can hand the tables over.

use crate::db::extend::ExtensionChain;
use crate::db::migrate::Migration;

macro_rules! extension {
    ($version:literal, $file:literal) => {
        Migration {
            version: $version,
            name: $file,
            sql: include_str!(concat!("../../../migrations/", $file)),
        }
    };
}

/// The provider-independent catalogue: what people ask for before the
/// library holds it, and the identities (ISRC, Spotify) it carries.
pub static CATALOG: ExtensionChain = ExtensionChain {
    name: "catalog",
    migrations: &[
        extension!(1, "catalog/0001_want_identities.sql"),
        extension!(2, "catalog/0002_playlist_entry_identities.sql"),
        extension!(3, "catalog/0003_spotify_playlist_syncs.sql"),
        extension!(4, "catalog/0004_entry_spotify_album.sql"),
        extension!(5, "catalog/0005_playlist_declines.sql"),
        extension!(6, "catalog/0006_album_provider_matches.sql"),
        extension!(7, "catalog/0007_artist_suggestions.sql"),
        extension!(8, "catalog/0008_artist_deezer_links.sql"),
        extension!(9, "catalog/0009_want_positions.sql"),
        extension!(10, "catalog/0010_source_identities.sql"),
        extension!(11, "catalog/0011_source_editions.sql"),
        extension!(12, "catalog/0012_source_catalogue.sql"),
    ],
};

/// Every extension chain, in the order they run.
pub static EXTENSIONS: &[&ExtensionChain] = &[&CATALOG];

#[cfg(test)]
mod tests {
    use super::EXTENSIONS;

    #[test]
    fn every_extension_file_keeps_to_its_own_tables() {
        for chain in EXTENSIONS {
            // Tables the chain's own earlier files created, which it may alter.
            let mut own: Vec<String> = Vec::new();
            for migration in chain.migrations {
                let sql = migration.sql.to_ascii_lowercase();
                for forbidden in ["user_version", "drop table"] {
                    assert!(
                        !sql.contains(forbidden),
                        "{}: {} must not use {forbidden}",
                        chain.name,
                        migration.name
                    );
                }
                for altered in words_after(&sql, "alter table") {
                    assert!(
                        own.contains(&altered),
                        "{}: {} alters {altered}, a table the chain did not create",
                        chain.name,
                        migration.name
                    );
                }
                own.extend(words_after(&sql, "create table"));
            }
        }
    }

    /// The table names that follow each `keyword` in `sql`.
    fn words_after(sql: &str, keyword: &str) -> Vec<String> {
        sql.match_indices(keyword)
            .filter_map(|(at, _)| {
                sql[at + keyword.len()..]
                    .split_whitespace()
                    .find(|word| !matches!(*word, "if" | "not" | "exists"))
                    .map(|word| word.trim_end_matches('(').to_owned())
            })
            .collect()
    }
}
