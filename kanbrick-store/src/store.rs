//! [`Store`] — the embedded SparrowDB lifecycle wrapper.
//!
//! Wraps a [`sparrowdb::GraphDb`] opened in file-backed mode and exposes the
//! query surface the rest of Kanbrick-V1 builds on:
//!
//! * [`Store::open`] / [`Store::close`] — lifecycle (issue #6).
//! * [`Store::execute`] — run a single Cypher statement.
//! * [`Store::query`] — parameterized, injection-safe, typed queries (issue #9).

use kanbrick_core::{Error, Result};
use serde::de::DeserializeOwned;
use sparrowdb::{GraphDb, QueryResult};

use crate::value::{value_to_json, Params};

/// Handle to the embedded firm graph store.
///
/// A `Store` owns one [`GraphDb`]. The underlying handle is `Send + Sync` and
/// follows SparrowDB's single-writer / multiple-reader model, so a `Store`
/// shared behind an [`std::sync::Arc`] supports any number of concurrent
/// readers without deadlock.
pub struct Store {
    db: GraphDb,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path()).finish()
    }
}

impl Store {
    /// Open (creating if absent) a file-backed store rooted at `path`.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let db = GraphDb::open(path.as_ref()).map_err(store_err)?;
        Ok(Store { db })
    }

    /// Filesystem location backing this store.
    pub fn path(&self) -> &std::path::Path {
        self.db.path()
    }

    /// Borrow the underlying SparrowDB handle, for operations not yet wrapped.
    pub fn graph(&self) -> &GraphDb {
        &self.db
    }

    /// Execute a single Cypher statement with no bound parameters.
    ///
    /// Suitable for DDL (constraints/indexes) and trusted internal statements.
    /// For anything carrying caller-supplied values, prefer [`Store::query`] or
    /// [`Store::execute_with`] so the values are bound rather than interpolated.
    pub fn execute(&self, cypher: &str) -> Result<QueryResult> {
        let start = std::time::Instant::now();
        let result = self.db.execute(cypher).map_err(query_err);
        trace_query(cypher, start, result.is_ok());
        result
    }

    /// Execute a Cypher statement with bound `params`.
    pub fn execute_with(&self, cypher: &str, params: Params) -> Result<QueryResult> {
        let start = std::time::Instant::now();
        let result = if params.is_empty() {
            self.db.execute(cypher)
        } else {
            self.db.execute_with_params(cypher, params.into_map())
        }
        .map_err(query_err);
        trace_query(cypher, start, result.is_ok());
        result
    }

    /// Run a parameterized query and deserialize each result row into `T`.
    ///
    /// Columns are matched to struct fields by name, so the `RETURN` clause
    /// should project explicit aliases (e.g. `RETURN p.email AS email`). Values
    /// are bound from `params`, never interpolated, so malicious input cannot
    /// change the query structure.
    pub fn query<T: DeserializeOwned>(&self, cypher: &str, params: Params) -> Result<Vec<T>> {
        let result = self.execute_with(cypher, params)?;
        rows_to_typed(&result)
    }

    /// Like [`Store::query`] but returns the first row, if any.
    pub fn query_one<T: DeserializeOwned>(
        &self,
        cypher: &str,
        params: Params,
    ) -> Result<Option<T>> {
        Ok(self.query::<T>(cypher, params)?.into_iter().next())
    }

    /// Convenience: run a query that returns a single integer cell (e.g. a
    /// `count(...)`), returning that integer.
    pub fn scalar_i64(&self, cypher: &str, params: Params) -> Result<Option<i64>> {
        let result = self.execute_with(cypher, params)?;
        match result.rows.first().and_then(|row| row.first()) {
            Some(cell) => match value_to_json(cell) {
                serde_json::Value::Number(n) => Ok(n.as_i64()),
                serde_json::Value::Null => Ok(None),
                other => Err(Error::Query(format!(
                    "expected integer scalar, got {other}"
                ))),
            },
            None => Ok(None),
        }
    }

    /// Flush pending writes to disk, making them durable across a reopen.
    pub fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().map_err(store_err)
    }

    /// Gracefully close the store: checkpoint, then drop the handle.
    ///
    /// Taking `self` by value guarantees the handle is released. Reopening the
    /// same path afterwards observes all checkpointed state.
    pub fn close(self) -> Result<()> {
        self.db.checkpoint().map_err(store_err)?;
        drop(self);
        Ok(())
    }
}

/// Deserialize every row of a [`QueryResult`] into `T` using the column names
/// as object keys.
fn rows_to_typed<T: DeserializeOwned>(result: &QueryResult) -> Result<Vec<T>> {
    let mut out = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let mut obj = serde_json::Map::with_capacity(result.columns.len());
        for (col, cell) in result.columns.iter().zip(row.iter()) {
            // A projection like `p.email AS email` yields the alias `email`;
            // an un-aliased `p.email` yields the column name `p.email`. Strip a
            // leading `<var>.` so both forms map onto the same struct field.
            let key = col.rsplit('.').next().unwrap_or(col).to_string();
            obj.insert(key, value_to_json(cell));
        }
        let value = serde_json::Value::Object(obj);
        let typed = serde_json::from_value(value)
            .map_err(|e| Error::Query(format!("row deserialization failed: {e}")))?;
        out.push(typed);
    }
    Ok(out)
}

/// Emit a structured trace event for an executed query and its duration.
fn trace_query(cypher: &str, start: std::time::Instant, ok: bool) {
    let micros = start.elapsed().as_micros();
    tracing::debug!(
        target: "kanbrick_store::query",
        duration_us = micros as u64,
        ok,
        cypher,
        "executed cypher query"
    );
}

/// Map a SparrowDB error from a query path into [`Error::Query`].
fn query_err(e: sparrowdb::Error) -> Error {
    Error::Query(e.to_string())
}

/// Map a SparrowDB error from a lifecycle path into [`Error::Store`].
fn store_err(e: sparrowdb::Error) -> Error {
    Error::Store(e.to_string())
}
