//! The runner for extension chains: schema a build adds beside the
//! upstream chain, each chain counted in its own `schema_extensions` row.
//! What may go in one, and why, is on [`crate::schema::extensions`].

use rusqlite::{Connection, TransactionBehavior, params};

use super::migrate::{Chain, Migration, SchemaError, migrate};
use super::sql;

/// One named chain of extension files, numbered from 1.
#[derive(Debug)]
pub struct ExtensionChain {
    /// The chain's row in `schema_extensions`.
    pub name: &'static str,
    /// Its files, in number order.
    pub migrations: &'static [Migration<'static>],
}

impl ExtensionChain {
    /// The newest file's number; 0 for an empty chain.
    #[must_use]
    pub fn latest(&self) -> i64 {
        self.migrations
            .last()
            .map_or(0, |migration| migration.version)
    }
}

/// Bring a database to `chain`'s stamp, then every chain in `extensions` to
/// its newest file. Hands the connection back; any failure closes it.
pub fn migrate_extended(
    connection: Connection,
    chain: &Chain<'_>,
    extensions: &[&ExtensionChain],
) -> Result<Connection, SchemaError> {
    let mut connection = migrate(connection, chain)?;
    apply(&mut connection, extensions)?;
    Ok(connection)
}

/// Open `path` with the canonical pragma set, bring it to `chain`'s stamp and
/// every extension to its newest file, then close it.
pub fn migrate_file_extended(
    path: &std::path::Path,
    chain: &Chain<'_>,
    extensions: &[&ExtensionChain],
) -> Result<(), super::DbError> {
    let connection = super::writer::open_connection(path)?;
    migrate_extended(connection, chain, extensions)?;
    Ok(())
}

/// Bring every chain in `chains` to its newest file.
pub fn apply(connection: &mut Connection, chains: &[&ExtensionChain]) -> Result<(), SchemaError> {
    if chains.iter().all(|chain| chain.migrations.is_empty()) {
        return Ok(());
    }
    connection.execute(sql::CREATE_EXTENSION_LEDGER.text(), [])?;
    for chain in chains {
        apply_chain(connection, chain)?;
    }
    Ok(())
}

/// A chain's count; 0 before its first file.
pub fn count(connection: &Connection, chain: &str) -> Result<i64, SchemaError> {
    use rusqlite::OptionalExtension as _;
    let ledger: bool =
        connection.query_row(sql::EXTENSION_LEDGER_PRESENT.text(), [], |row| row.get(0))?;
    if !ledger {
        return Ok(0);
    }
    Ok(connection
        .query_row(sql::READ_EXTENSION_COUNT.text(), params![chain], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or(0))
}

fn apply_chain(connection: &mut Connection, chain: &ExtensionChain) -> Result<(), SchemaError> {
    let found = count(connection, chain.name)?;
    if found > chain.latest() {
        return Err(SchemaError::ExtensionTooNew {
            chain: chain.name,
            found,
            latest: chain.latest(),
        });
    }
    for migration in chain.migrations.iter().filter(|m| m.version > found) {
        let version = migration.version;
        let failed = |source| SchemaError::Failed { version, source };
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(failed)?;
        transaction.execute_batch(migration.sql).map_err(failed)?;
        transaction
            .execute(
                sql::WRITE_EXTENSION_COUNT.text(),
                params![chain.name, version],
            )
            .map_err(failed)?;
        transaction.commit().map_err(failed)?;
        tracing::info!(
            chain = chain.name,
            version,
            name = migration.name,
            "applied a schema extension"
        );
    }
    Ok(())
}
