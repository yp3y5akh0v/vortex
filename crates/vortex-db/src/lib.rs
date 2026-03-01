//! vortex-db: Custom PostgreSQL wire protocol client for Vortex.
//!
//! Implements the minimum PostgreSQL protocol needed for TechEmpower:
//! - Startup + authentication (trust, cleartext, MD5, SCRAM-SHA-256)
//! - Prepared statements (Parse/Bind/Execute/Sync)
//! - Binary format for parameters and results
//! - Pipelined queries for /queries and /updates
//! - Per-thread connection pooling

pub mod wire;
pub mod scram;
pub mod connection;
pub mod pool;

pub use connection::{DbConfig, PgConnection};
pub use pool::PgPool;

/// Generate a random World ID (1..=10000).
#[inline]
pub fn random_world_id() -> i32 {
    use nanorand::Rng;
    let mut rng = nanorand::WyRand::new();
    (rng.generate_range(0_u32..10000) + 1) as i32
}

/// Clamp a queries parameter to [1, 500].
#[inline]
pub fn clamp_queries(n: i32) -> i32 {
    if n < 1 { 1 } else if n > 500 { 500 } else { n }
}
