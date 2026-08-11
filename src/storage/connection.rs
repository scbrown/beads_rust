//! Synchronous bridge over fsqlite 0.2's async connection API.
//!
//! fsqlite 0.2 made every engine entry point `async fn` returning `!Send`
//! futures (the engine lives in `Rc<RefCell<>>`). beads_rust is a synchronous
//! CLI whose entire storage layer — roughly 800 call sites across
//! `storage::sqlite`, `storage::schema`, `storage::events`, the doctor
//! subsystems, and the command layer — calls `execute`/`query`/`prepare`
//! directly on a shared `&Connection`. Rather than propagating `async`
//! through all of that, this module keeps the 0.1.x synchronous surface:
//!
//! - [`Connection`] owns the underlying [`fsqlite::Connection`] **and** a
//!   private current-thread [`asupersync::runtime::Runtime`].
//! - Every fsqlite future is created, polled to completion via
//!   [`Runtime::block_on`], and dropped within a single method call on the
//!   calling thread, so the engine's `Rc` state never crosses a thread
//!   boundary. `fsqlite::Connection` was already `!Send` in 0.1.x, so this
//!   changes nothing about how the type may be shared.
//! - Close paths destructure the wrapper and drive `close()` /
//!   `close_in_place()` on the owned runtime.
//! - `Runtime::block_on` has no `Send` bound and saves/restores the ambient
//!   runtime handle, so nested use from inside another runtime's `block_on`
//!   or a worker thread is safe (probed by the reference migration in
//!   sqlmodel_rust's `nested_block_on_*` tests).
//!
//! Method names and signatures deliberately mirror the fsqlite 0.1.x
//! `Connection` so existing call sites compile unchanged against this
//! wrapper.

// fsqlite's statement futures are large (~19 KiB), but every one is driven to
// completion immediately on the blocking thread's stack via `block_on`; they
// are never embedded in another future, so boxing would only add churn.
#![allow(clippy::large_futures)]

use asupersync::runtime::{Runtime, RuntimeBuilder};
use fsqlite_error::FrankenError;
use fsqlite_types::SqliteValue;

pub use fsqlite::Row;
pub use fsqlite::compat::OpenFlags;

type Result<T> = std::result::Result<T, FrankenError>;

/// Build the private current-thread runtime that drives fsqlite futures.
fn build_driver_runtime() -> Result<Runtime> {
    RuntimeBuilder::current_thread().build().map_err(|error| {
        FrankenError::Internal(format!("failed to build fsqlite driver runtime: {error}"))
    })
}

/// Synchronous wrapper around [`fsqlite::Connection`] (see module docs).
pub struct Connection {
    inner: fsqlite::Connection,
    /// Private current-thread runtime that drives the engine's `!Send`
    /// futures to completion inside each synchronous method call.
    runtime: Runtime,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Open (creating if necessary) a database at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the driver runtime cannot be built or the engine
    /// fails to open the database.
    pub fn open(path: impl Into<String>) -> Result<Self> {
        let runtime = build_driver_runtime()?;
        let inner = runtime.block_on(fsqlite::Connection::open(path))?;
        Ok(Self { inner, runtime })
    }

    /// Drive a `!Send` fsqlite future to completion on the calling thread.
    ///
    /// The future is created, polled, and dropped entirely within the
    /// enclosing method call, so the engine's `Rc<RefCell<>>` state never
    /// crosses a thread boundary.
    fn drive<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }

    /// Execute a statement, returning the number of affected rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails to parse or execute.
    pub fn execute(&self, sql: &str) -> Result<usize> {
        self.drive(self.inner.execute(sql))
    }

    /// Execute a statement with bound parameters.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails to parse or execute.
    pub fn execute_with_params(&self, sql: &str, params: &[SqliteValue]) -> Result<usize> {
        self.drive(self.inner.execute_with_params(sql, params))
    }

    /// Run a query and collect all result rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails to parse or execute.
    pub fn query(&self, sql: &str) -> Result<Vec<Row>> {
        self.drive(self.inner.query(sql))
    }

    /// Run a query with bound parameters and collect all result rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails to parse or execute.
    pub fn query_with_params(&self, sql: &str, params: &[SqliteValue]) -> Result<Vec<Row>> {
        self.drive(self.inner.query_with_params(sql, params))
    }

    /// Run a query expected to produce exactly one row.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or does not produce exactly one
    /// row.
    pub fn query_row(&self, sql: &str) -> Result<Row> {
        self.drive(self.inner.query_row(sql))
    }

    /// Run a parameterized query expected to produce exactly one row.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails or does not produce exactly one
    /// row.
    pub fn query_row_with_params(&self, sql: &str, params: &[SqliteValue]) -> Result<Row> {
        self.drive(self.inner.query_row_with_params(sql, params))
    }

    /// Prepare a statement for repeated execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the statement fails to parse.
    pub fn prepare(&self, sql: &str) -> Result<PreparedStatement<'_>> {
        let inner = self.drive(self.inner.prepare(sql))?;
        Ok(PreparedStatement {
            runtime: &self.runtime,
            inner,
        })
    }

    /// Close the connection, checkpointing the WAL.
    ///
    /// # Errors
    ///
    /// Returns an error if the close (including its WAL checkpoint) fails.
    pub fn close(self) -> Result<()> {
        let Self { inner, runtime } = self;
        runtime.block_on(inner.close())
    }

    /// Close the connection in place (used from `Drop` paths).
    ///
    /// # Errors
    ///
    /// Returns an error if the close fails.
    pub fn close_in_place(&mut self) -> Result<()> {
        let Self { inner, runtime } = self;
        runtime.block_on(inner.close_in_place())
    }
}

/// Synchronous wrapper around [`fsqlite::PreparedStatement`], borrowed from a
/// [`Connection`]. Drives the statement's async methods on the connection's
/// runtime.
pub struct PreparedStatement<'conn> {
    runtime: &'conn Runtime,
    inner: fsqlite::PreparedStatement<'conn>,
}

impl std::fmt::Debug for PreparedStatement<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedStatement")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl PreparedStatement<'_> {
    /// Render the compiled program for diagnostics (sync in fsqlite).
    #[must_use]
    pub fn explain(&self) -> String {
        self.inner.explain()
    }

    /// Execute as a query and return all result rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails to execute.
    pub fn query(&self) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.query())
    }

    /// Execute as a query with bound parameters and return all result rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails to execute.
    pub fn query_with_params(&self, params: &[SqliteValue]) -> Result<Vec<Row>> {
        self.runtime.block_on(self.inner.query_with_params(params))
    }
}

/// Open a database with SQLite-style open flags (compat surface).
///
/// # Errors
///
/// Returns an error if the driver runtime cannot be built or the engine
/// fails to open the database.
pub fn open_with_flags(path: &str, flags: OpenFlags) -> Result<Connection> {
    let runtime = build_driver_runtime()?;
    let inner = runtime.block_on(fsqlite::compat::open_with_flags(path, flags))?;
    Ok(Connection { inner, runtime })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_execute_query_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "beads_bridge_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bridge.db");
        let conn = Connection::open(path.to_string_lossy().into_owned()).unwrap();
        conn.execute("CREATE TABLE t (k TEXT PRIMARY KEY, v INTEGER)")
            .unwrap();
        let n = conn
            .execute_with_params(
                "INSERT INTO t (k, v) VALUES (?, ?)",
                &[SqliteValue::from("a"), SqliteValue::Integer(7)],
            )
            .unwrap();
        assert_eq!(n, 1);

        let rows = conn
            .query_with_params("SELECT v FROM t WHERE k = ?", &[SqliteValue::from("a")])
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0).and_then(SqliteValue::as_integer), Some(7));

        let row = conn.query_row("SELECT count(*) FROM t").unwrap();
        assert_eq!(row.get(0).and_then(SqliteValue::as_integer), Some(1));

        let stmt = conn.prepare("SELECT v FROM t WHERE k = ?").unwrap();
        let rows = stmt.query_with_params(&[SqliteValue::from("a")]).unwrap();
        assert_eq!(rows.len(), 1);
        drop(stmt);

        conn.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nested_block_on_inside_outer_runtime_is_safe() {
        // KEY RISK probe mirrored from the sqlmodel_rust reference migration:
        // the sync bridge must keep working when called from inside another
        // asupersync runtime's block_on (e.g. a consumer embedding br).
        let outer = RuntimeBuilder::current_thread().build().unwrap();
        let value = outer.block_on(async {
            let dir = std::env::temp_dir().join(format!(
                "beads_bridge_nested_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("nested.db");
            let conn = Connection::open(path.to_string_lossy().into_owned()).unwrap();
            conn.execute("CREATE TABLE n (v INTEGER)").unwrap();
            conn.execute("INSERT INTO n (v) VALUES (41), (1)").unwrap();
            let row = conn.query_row("SELECT sum(v) FROM n").unwrap();
            let sum = row.get(0).and_then(SqliteValue::as_integer);
            conn.close().unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            sum
        });
        assert_eq!(value, Some(42));
    }
}
