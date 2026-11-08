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
            for migration in chain.migrations {
                let sql = migration.sql.to_ascii_lowercase();
                for forbidden in ["user_version", "alter table", "drop table"] {
                    assert!(
                        !sql.contains(forbidden),
                        "{}: {} must not use {forbidden}",
                        chain.name,
                        migration.name
                    );
                }
            }
        }
    }
}
