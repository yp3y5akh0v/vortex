//! vortex-server: HTTP server combining all Vortex layers.
//!
//! Provides the Server builder API that wires vortex-io, vortex-runtime,
//! and vortex-http into a working HTTP server.

pub mod app;
pub mod server;

pub use app::{App, RouteAction};
pub use server::Server;

// Re-export sub-crates for application use.
pub use vortex_http as http;
pub use vortex_json as json;
pub use vortex_template as template;
pub use vortex_db as db;
