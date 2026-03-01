//! Server builder and main entry point.

use vortex_io::common::affinity;
use vortex_io::common::socket;
use vortex_io::uring::multishot;
use vortex_io::uring::ring::{Ring, RingConfig};
use vortex_http::date::DateCache;
use vortex_http::parser::{self, Route};
use vortex_http::pipeline;
use vortex_http::response::{DynHtmlResponse, DynJsonResponse};
use vortex_db::{DbConfig, PgPool};
use std::io;

/// Vortex HTTP server.
pub struct Server {
    addr: String,
    port: u16,
    workers: usize,
    backlog: i32,
    sqpoll: bool,
}

impl Server {
    /// Create a new server builder.
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            addr: "0.0.0.0".to_string(),
            port: 8080,
            workers: 0,
            backlog: 4096,
            sqpoll: false,
        }
    }
}

/// Builder for configuring and launching the server.
pub struct ServerBuilder {
    addr: String,
    port: u16,
    workers: usize,
    backlog: i32,
    sqpoll: bool,
}

impl ServerBuilder {
    pub fn addr(mut self, addr: &str) -> Self {
        self.addr = addr.to_string();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n;
        self
    }

    pub fn backlog(mut self, n: i32) -> Self {
        self.backlog = n;
        self
    }

    pub fn sqpoll(mut self, enabled: bool) -> Self {
        self.sqpoll = enabled;
        self
    }

    /// Build and run the server, blocking until all workers complete.
    pub fn run(self) -> io::Result<()> {
        let env_workers: usize = std::env::var("VORTEX_WORKERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let num_workers = if env_workers > 0 {
            env_workers
        } else if self.workers > 0 {
            self.workers
        } else {
            affinity::available_cores()
        };

        let sqpoll = std::env::var("VORTEX_SQPOLL")
            .map(|s| s == "1" || s == "true")
            .unwrap_or(self.sqpoll);

        // Database config (read once, shared by reference)
        let db_config = DbConfig::from_env();
        let db_conns: usize = std::env::var("DB_POOL_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);

        // Resolve DB address once on main thread to avoid DNS thundering herd
        let db_addr = match vortex_db::PgConnection::resolve_host(&db_config) {
            Ok(addr) => {
                eprintln!("[vortex] DB resolved to {}", addr);
                Some(addr)
            }
            Err(e) => {
                eprintln!("[vortex] DB DNS resolution failed: {} (DB endpoints disabled)", e);
                None
            }
        };

        eprintln!(
            "[vortex] Starting {} workers on {}:{} (sqpoll={})",
            num_workers, self.addr, self.port, sqpoll
        );

        let mut handles = Vec::with_capacity(num_workers);

        for core_id in 0..num_workers {
            let addr = self.addr.clone();
            let port = self.port;
            let backlog = self.backlog;
            let db_cfg = DbConfig {
                host: db_config.host.clone(),
                port: db_config.port,
                user: db_config.user.clone(),
                password: db_config.password.clone(),
                database: db_config.database.clone(),
            };
            let db_resolved = db_addr;

            let handle = std::thread::Builder::new()
                .name(format!("vortex-w{}", core_id))
                .spawn(move || {
                    worker_main(core_id, num_workers, &addr, port, backlog, sqpoll, &db_cfg, db_conns, db_resolved)
                })?;
            handles.push(handle);
        }

        for handle in handles {
            if let Err(e) = handle.join().unwrap() {
                eprintln!("[vortex] Worker error: {}", e);
            }
        }

        Ok(())
    }
}

/// Token types encoded in io_uring user_data.
const TOKEN_ACCEPT: u64 = 0;
const TOKEN_RECV_BASE: u64 = 1 << 32;
const TOKEN_SEND_BASE: u64 = 2 << 32;
const TOKEN_CLOSE: u64 = u64::MAX;

/// Per-connection state.
struct Connection {
    fd: i32,
    recv_buf: Vec<u8>,
    send_buf: Vec<u8>,
}

const RECV_BUF_SIZE: usize = 4096;
const SEND_BUF_SIZE: usize = 65536;

/// Scratch buffer for building JSON/HTML bodies before copying into send_buf.
const BODY_BUF_SIZE: usize = 32768;

/// Reusable buffers for DB endpoint handlers (eliminates per-request allocations).
struct WorkerBufs {
    worlds: Vec<(i32, i32)>,
    random_numbers: Vec<i32>,
    fortunes: Vec<(i32, String)>,
    ids: Vec<i32>,
    html: Vec<u8>,
}

impl WorkerBufs {
    fn new() -> Self {
        Self {
            worlds: Vec::with_capacity(500),
            random_numbers: Vec::with_capacity(500),
            fortunes: Vec::with_capacity(16),
            ids: Vec::with_capacity(500),
            html: Vec::with_capacity(4096),
        }
    }
}

/// Main event loop for a single worker thread.
fn worker_main(
    core_id: usize,
    num_workers: usize,
    addr: &str,
    port: u16,
    backlog: i32,
    sqpoll: bool,
    db_config: &DbConfig,
    db_pool_size: usize,
    db_addr: Option<std::net::SocketAddr>,
) -> io::Result<()> {
    let _ = affinity::pin_to_core(core_id);

    let config = RingConfig {
        sq_entries: 4096,
        sqpoll,
        sqpoll_idle_ms: 1000,
    };
    let mut ring = Ring::new(&config)?;

    let listener_fd = socket::create_listener(addr, port, backlog)?;

    // Attach BPF to route connections by CPU (non-fatal on failure)
    if let Err(_e) = socket::attach_reuseport_cbpf(listener_fd, num_workers) {
        eprintln!("[vortex] Worker {} BPF attach failed (non-fatal)", core_id);
    }
    let mut date = DateCache::new();

    // Try to connect to PostgreSQL (optional — may not be available)
    let mut db_pool = if let Some(resolved) = db_addr {
        std::thread::sleep(std::time::Duration::from_millis(core_id as u64 * 10));
        match PgPool::new_resolved(resolved, db_config, db_pool_size) {
            Ok(pool) => {
                eprintln!("[vortex] Worker {} connected to DB ({} connections)", core_id, pool.len());
                Some(pool)
            }
            Err(e) => {
                eprintln!("[vortex] Worker {} DB connect failed: {} (DB endpoints disabled)", core_id, e);
                None
            }
        }
    } else {
        None
    };

    let mut connections: Vec<Option<Connection>> = Vec::new();
    connections.resize_with(65536, || None);

    // Reusable scratch buffers
    let mut body_buf = vec![0u8; BODY_BUF_SIZE];
    let mut bufs = WorkerBufs::new();

    unsafe {
        let sqe = multishot::prep_multishot_accept(listener_fd, TOKEN_ACCEPT);
        ring.push_sqe(&sqe).map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "SQ full")
        })?;
    }
    ring.submit()?;

    eprintln!("[vortex] Worker {} listening on fd {}", core_id, listener_fd);

    let mut cqes: Vec<(u64, i32)> = Vec::with_capacity(256);

    loop {
        date.maybe_update();
        ring.submit_and_wait(1)?;

        cqes.clear();
        for cqe in ring.completions() {
            cqes.push((cqe.user_data(), cqe.result()));
        }

        for &(user_data, result) in &cqes {
            if user_data == TOKEN_ACCEPT {
                if result >= 0 {
                    let conn_fd = result;
                    let _ = socket::configure_accepted(conn_fd);

                    let fd_idx = conn_fd as usize;
                    if fd_idx >= connections.len() {
                        connections.resize_with(fd_idx + 1024, || None);
                    }

                    connections[fd_idx] = Some(Connection {
                        fd: conn_fd,
                        recv_buf: vec![0u8; RECV_BUF_SIZE],
                        send_buf: vec![0u8; SEND_BUF_SIZE],
                    });

                    if let Some(conn) = &mut connections[fd_idx] {
                        unsafe {
                            let sqe = multishot::prep_recv(
                                conn_fd,
                                conn.recv_buf.as_mut_ptr(),
                                conn.recv_buf.len() as u32,
                                TOKEN_RECV_BASE | conn_fd as u64,
                            );
                            let _ = ring.push_sqe(&sqe);
                        }
                    }
                }
            } else if (user_data >> 32) == 1 {
                // Recv completion
                let conn_fd = (user_data & 0xFFFFFFFF) as i32;
                let fd_idx = conn_fd as usize;

                if result <= 0 {
                    close_connection(&mut connections, fd_idx, &mut ring)?;
                } else {
                    let len = result as usize;

                    let route = if let Some(conn) = &connections[fd_idx] {
                        parser::classify_fast(&conn.recv_buf[..len])
                    } else {
                        Route::NotFound
                    };

                    let resp_len = match route {
                        Route::Plaintext | Route::Json | Route::NotFound => {
                            if let Some(conn) = &mut connections[fd_idx] {
                                let (_count, rlen) = pipeline::process_pipelined(
                                    &conn.recv_buf[..len],
                                    &mut conn.send_buf,
                                    &date,
                                );
                                rlen
                            } else {
                                0
                            }
                        }

                        Route::Db => {
                            handle_db(&mut connections, fd_idx, &date, &mut db_pool, &mut body_buf)
                        }
                        Route::Queries => {
                            let queries = if let Some(conn) = &connections[fd_idx] {
                                vortex_db::clamp_queries(parser::parse_queries_param(&conn.recv_buf[..len]))
                            } else {
                                1
                            };
                            handle_queries(&mut connections, fd_idx, &date, &mut db_pool, &mut body_buf, queries, &mut bufs)
                        }
                        Route::Fortunes => {
                            handle_fortunes(&mut connections, fd_idx, &date, &mut db_pool, &mut bufs)
                        }
                        Route::Updates => {
                            let queries = if let Some(conn) = &connections[fd_idx] {
                                vortex_db::clamp_queries(parser::parse_queries_param(&conn.recv_buf[..len]))
                            } else {
                                1
                            };
                            handle_updates(&mut connections, fd_idx, &date, &mut db_pool, &mut body_buf, queries, &mut bufs)
                        }
                    };

                    if resp_len > 0 {
                        if let Some(conn) = &connections[fd_idx] {
                            unsafe {
                                let sqe = multishot::prep_send(
                                    conn_fd,
                                    conn.send_buf.as_ptr(),
                                    resp_len as u32,
                                    TOKEN_SEND_BASE | conn_fd as u64,
                                );
                                let _ = ring.push_sqe(&sqe);
                            }
                        }
                    } else {
                        close_connection(&mut connections, fd_idx, &mut ring)?;
                    }
                }
            } else if (user_data >> 32) == 2 {
                // Send completion
                let conn_fd = (user_data & 0xFFFFFFFF) as i32;
                let fd_idx = conn_fd as usize;

                if result < 0 {
                    close_connection(&mut connections, fd_idx, &mut ring)?;
                } else {
                    if let Some(conn) = &mut connections[fd_idx] {
                        unsafe {
                            let sqe = multishot::prep_recv(
                                conn_fd,
                                conn.recv_buf.as_mut_ptr(),
                                conn.recv_buf.len() as u32,
                                TOKEN_RECV_BASE | conn_fd as u64,
                            );
                            let _ = ring.push_sqe(&sqe);
                        }
                    }
                }
            }
        }
    }
}

// ── DB endpoint handlers ─────────────────────────────────────────────

/// Handle /db — single random world row.
fn handle_db(
    connections: &mut [Option<Connection>],
    fd_idx: usize,
    date: &DateCache,
    db_pool: &mut Option<PgPool>,
    body_buf: &mut [u8],
) -> usize {
    let pool = match db_pool.as_mut() {
        Some(p) => p,
        None => return write_503(connections, fd_idx),
    };

    let id = vortex_db::random_world_id();
    let conn = pool.get();
    match conn.query_world(id) {
        Ok((world_id, random_number)) => {
            let body_len = vortex_json::write_world(body_buf, world_id, random_number);
            if let Some(http_conn) = &mut connections[fd_idx] {
                DynJsonResponse::write(&mut http_conn.send_buf, date, &body_buf[..body_len])
            } else {
                0
            }
        }
        Err(_) => write_500(connections, fd_idx),
    }
}

/// Handle /queries?queries=N — N random world rows.
fn handle_queries(
    connections: &mut [Option<Connection>],
    fd_idx: usize,
    date: &DateCache,
    db_pool: &mut Option<PgPool>,
    body_buf: &mut [u8],
    queries: i32,
    bufs: &mut WorkerBufs,
) -> usize {
    let pool = match db_pool.as_mut() {
        Some(p) => p,
        None => return write_503(connections, fd_idx),
    };

    bufs.ids.clear();
    for _ in 0..queries {
        bufs.ids.push(vortex_db::random_world_id());
    }

    let conn = pool.get();
    match conn.query_worlds(&bufs.ids, &mut bufs.worlds) {
        Ok(()) => {
            let body_len = vortex_json::write_worlds(body_buf, &bufs.worlds);
            if let Some(http_conn) = &mut connections[fd_idx] {
                DynJsonResponse::write(&mut http_conn.send_buf, date, &body_buf[..body_len])
            } else {
                0
            }
        }
        Err(_) => write_500(connections, fd_idx),
    }
}

/// Handle /fortunes — HTML table of all fortunes.
fn handle_fortunes(
    connections: &mut [Option<Connection>],
    fd_idx: usize,
    date: &DateCache,
    db_pool: &mut Option<PgPool>,
    bufs: &mut WorkerBufs,
) -> usize {
    let pool = match db_pool.as_mut() {
        Some(p) => p,
        None => return write_503(connections, fd_idx),
    };

    let conn = pool.get();
    match conn.query_fortunes(&mut bufs.fortunes) {
        Ok(()) => {
            vortex_template::render_fortunes(&bufs.fortunes, &mut bufs.html);
            if let Some(http_conn) = &mut connections[fd_idx] {
                DynHtmlResponse::write(&mut http_conn.send_buf, date, &bufs.html)
            } else {
                0
            }
        }
        Err(_) => write_500(connections, fd_idx),
    }
}

/// Handle /updates?queries=N — read N rows, update with new random values.
fn handle_updates(
    connections: &mut [Option<Connection>],
    fd_idx: usize,
    date: &DateCache,
    db_pool: &mut Option<PgPool>,
    body_buf: &mut [u8],
    queries: i32,
    bufs: &mut WorkerBufs,
) -> usize {
    let pool = match db_pool.as_mut() {
        Some(p) => p,
        None => return write_503(connections, fd_idx),
    };

    // Generate random IDs
    bufs.ids.clear();
    for _ in 0..queries {
        bufs.ids.push(vortex_db::random_world_id());
    }

    // Read N random worlds
    let conn = pool.get();
    if conn.query_worlds(&bufs.ids, &mut bufs.worlds).is_err() {
        return write_500(connections, fd_idx);
    }

    // Build sorted ids and new random numbers for batch update
    bufs.ids.clear();
    bufs.random_numbers.clear();
    for &(id, _old_rn) in &bufs.worlds {
        bufs.ids.push(id);
        bufs.random_numbers.push(vortex_db::random_world_id());
    }
    bufs.ids.sort_unstable(); // sorted ids reduce lock contention in PostgreSQL

    // Execute single batch UPDATE via unnest()
    let conn = pool.get();
    if conn.update_worlds_batch(&bufs.ids, &bufs.random_numbers).is_err() {
        return write_500(connections, fd_idx);
    }

    // Build result: (id, new_randomNumber)
    bufs.worlds.clear();
    for i in 0..bufs.ids.len() {
        bufs.worlds.push((bufs.ids[i], bufs.random_numbers[i]));
    }

    let body_len = vortex_json::write_worlds(body_buf, &bufs.worlds);
    if let Some(http_conn) = &mut connections[fd_idx] {
        DynJsonResponse::write(&mut http_conn.send_buf, date, &body_buf[..body_len])
    } else {
        0
    }
}

// ── Error responses ──────────────────────────────────────────────────

fn write_500(connections: &mut [Option<Connection>], fd_idx: usize) -> usize {
    const RESP: &[u8] = b"HTTP/1.1 500 Internal Server Error\r\nServer: V\r\nContent-Length: 0\r\n\r\n";
    if let Some(conn) = &mut connections[fd_idx] {
        conn.send_buf[..RESP.len()].copy_from_slice(RESP);
        RESP.len()
    } else {
        0
    }
}

fn write_503(connections: &mut [Option<Connection>], fd_idx: usize) -> usize {
    const RESP: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\nServer: V\r\nContent-Length: 0\r\n\r\n";
    if let Some(conn) = &mut connections[fd_idx] {
        conn.send_buf[..RESP.len()].copy_from_slice(RESP);
        RESP.len()
    } else {
        0
    }
}

fn close_connection(
    connections: &mut [Option<Connection>],
    fd_idx: usize,
    ring: &mut Ring,
) -> io::Result<()> {
    if fd_idx < connections.len() {
        if let Some(conn) = connections[fd_idx].take() {
            unsafe {
                let sqe = multishot::prep_close(conn.fd, TOKEN_CLOSE);
                let _ = ring.push_sqe(&sqe);
            }
        }
    }
    Ok(())
}
