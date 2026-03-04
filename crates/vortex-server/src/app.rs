//! Application trait for defining benchmark/server behavior.

use vortex_http::date::DateCache;

/// Route classification result.
pub enum RouteAction {
    /// Fast path — no DB needed (e.g. plaintext, json).
    Fast(u8),
    /// DB path — needs async database operation.
    Db { id: u8, queries: i32 },
    /// Not found.
    NotFound,
}

/// Application trait. Implement this to define routes and handlers.
///
/// All methods are monomorphized at compile time — zero runtime overhead.
pub trait App: Send + 'static {
    /// Per-DB-connection application state.
    type DbState: Default + Send;

    /// Classify an HTTP request buffer into a route action.
    fn classify(buf: &[u8]) -> RouteAction;

    /// Handle a fast route (no DB). Returns (requests_processed, bytes_written).
    fn handle_fast(id: u8, recv: &[u8], send: &mut [u8], date: &DateCache) -> (usize, usize);

    /// Prepared statements to create at DB connection startup.
    /// Each entry: (name, sql, param_oids).
    fn db_statements() -> Vec<(&'static str, &'static str, &'static [u32])>;

    /// Start a DB operation: write PG wire protocol messages to wbuf.
    fn db_start(id: u8, queries: i32, wbuf: &mut Vec<u8>, state: &mut Self::DbState);

    /// Process DB response: parse rbuf, write HTTP response to send_buf.
    /// Returns response bytes written.
    fn db_finish(
        state: &mut Self::DbState,
        rbuf: &[u8],
        rpos: usize,
        send: &mut [u8],
        date: &DateCache,
        body: &mut [u8],
    ) -> usize;
}
