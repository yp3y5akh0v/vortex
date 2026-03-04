//! Server builder and main entry point.

use vortex_io::common::affinity;
use vortex_io::common::socket;
use vortex_io::uring::multishot;
use vortex_io::uring::registered;
use vortex_io::uring::bufring::ProvidedBufRing;
use vortex_io::uring::filetable::FileTable;
use vortex_io::uring::ring::{Ring, RingConfig};
use vortex_http::date::DateCache;
use vortex_db::{DbConfig, PgConnection};
use crate::app::{App, RouteAction};
use std::collections::VecDeque;
use std::io;
use std::marker::PhantomData;

/// Vortex HTTP server.
pub struct Server<A: App>(PhantomData<A>);

impl<A: App> Server<A> {
    /// Create a new server builder.
    pub fn builder() -> ServerBuilder<A> {
        ServerBuilder {
            addr: "0.0.0.0".to_string(),
            port: 8080,
            workers: 0,
            backlog: 4096,
            sqpoll: false,
            _app: PhantomData,
        }
    }
}

/// Builder for configuring and launching the server.
pub struct ServerBuilder<A: App> {
    addr: String,
    port: u16,
    workers: usize,
    backlog: i32,
    sqpoll: bool,
    _app: PhantomData<A>,
}

impl<A: App> ServerBuilder<A> {
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
        let db_pool_size: usize = std::env::var("DB_POOL_SIZE")
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

        // Collect statements once (they're the same for all workers)
        let stmts = A::db_statements();

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
            let stmts = stmts.clone();
            let db_pool = db_pool_size;

            let handle = std::thread::Builder::new()
                .name(format!("vortex-w{}", core_id))
                .spawn(move || {
                    worker_main::<A>(core_id, num_workers, &addr, port, backlog, sqpoll, &db_cfg, db_pool, db_resolved, &stmts)
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

// ── Token types encoded in io_uring user_data ───────────────────────
// Lower 32 bits encode the slot/index.

const TOKEN_ACCEPT: u64 = 0;
const TOKEN_RECV_BASE: u64 = 1 << 32;
const TOKEN_SEND_BASE: u64 = 2 << 32;
const TOKEN_CLOSE_BASE: u64 = 3 << 32;
const TOKEN_DB_SEND: u64 = 4 << 32;
const TOKEN_DB_RECV: u64 = 5 << 32;

// ── Per-connection state ────────────────────────────────────────────

struct Connection {
    send_buf: Vec<u8>,
}

const SEND_BUF_SIZE: usize = 65536;
const BODY_BUF_SIZE: usize = 32768;
const DB_BUF_SIZE: usize = 32768;

// ── Async DB types ──────────────────────────────────────────────────

struct AsyncDbConn<S> {
    slot: u32,
    wbuf: Vec<u8>,
    rbuf: Vec<u8>,
    rpos: usize,
    idle: bool,
    http_slot: u32,
    state: S,
}

struct DbRequest {
    http_slot: u32,
    route_id: u8,
    queries: i32,
}

// ── Worker main ─────────────────────────────────────────────────────

fn worker_main<A: App>(
    core_id: usize,
    num_workers: usize,
    addr: &str,
    port: u16,
    backlog: i32,
    sqpoll: bool,
    db_config: &DbConfig,
    db_pool_size: usize,
    db_addr: Option<std::net::SocketAddr>,
    stmts: &[(&str, &str, &[u32])],
) -> io::Result<()> {
    let _ = affinity::pin_to_core(core_id);

    let config = RingConfig {
        sq_entries: 4096,
        sqpoll,
        sqpoll_idle_ms: 1000,
    };
    let mut ring = Ring::new(&config)?;

    let file_table_cap = 4096u32;
    if let Err(e) = registered::register_files_sparse(&ring.submitter(), file_table_cap) {
        eprintln!("[vortex] Worker {} register_files_sparse failed: {} (falling back to Fd)", core_id, e);
    }
    let mut file_table = FileTable::new(file_table_cap);

    let buf_ring = ProvidedBufRing::new(
        &ring.submitter(),
        0,
        ProvidedBufRing::DEFAULT_BUF_COUNT,
        ProvidedBufRing::DEFAULT_BUF_SIZE,
    )?;

    let listener_fd = socket::create_listener(addr, port, backlog)?;

    if let Err(_e) = socket::attach_reuseport_cbpf(listener_fd, num_workers) {
        eprintln!("[vortex] Worker {} BPF attach failed (non-fatal)", core_id);
    }
    let mut date = DateCache::new();

    // ── Async DB connection pool ────────────────────────────────────
    let mut db_conns: Vec<AsyncDbConn<A::DbState>> = Vec::with_capacity(db_pool_size);
    let mut db_queue: VecDeque<DbRequest> = VecDeque::with_capacity(256);

    if let Some(resolved) = db_addr {
        std::thread::sleep(std::time::Duration::from_millis(core_id as u64 * 10));
        for _ in 0..db_pool_size {
            match PgConnection::connect_resolved_with_stmts(resolved, db_config, stmts) {
                Ok(pg_conn) => {
                    let raw_fd = pg_conn.into_raw_fd();
                    unsafe {
                        let flags = libc::fcntl(raw_fd, libc::F_GETFL);
                        libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                    }
                    let slot = match file_table.alloc() {
                        Some(s) => s,
                        None => { unsafe { libc::close(raw_fd); } break; }
                    };
                    if registered::update_file(&ring.submitter(), slot, raw_fd).is_err() {
                        file_table.free(slot);
                        unsafe { libc::close(raw_fd); }
                        break;
                    }
                    unsafe { libc::close(raw_fd); }
                    db_conns.push(AsyncDbConn {
                        slot,
                        wbuf: Vec::with_capacity(DB_BUF_SIZE),
                        rbuf: vec![0u8; DB_BUF_SIZE],
                        rpos: 0,
                        idle: true,
                        http_slot: 0,
                        state: A::DbState::default(),
                    });
                }
                Err(e) => {
                    eprintln!("[vortex] Worker {} DB connect failed: {} (DB endpoints disabled)", core_id, e);
                    break;
                }
            }
        }
        if !db_conns.is_empty() {
            eprintln!("[vortex] Worker {} connected to DB ({} connections)", core_id, db_conns.len());
        } else {
            eprintln!("[vortex] Worker {} DB not available (DB endpoints disabled)", core_id);
        }
    }

    let mut connections: Vec<Option<Connection>> = Vec::new();
    connections.resize_with(file_table_cap as usize, || None);

    let mut body_buf = vec![0u8; BODY_BUF_SIZE];
    let mut send_buf_pool: Vec<Vec<u8>> = (0..256).map(|_| vec![0u8; SEND_BUF_SIZE]).collect();

    unsafe {
        let sqe = multishot::prep_multishot_accept(listener_fd, TOKEN_ACCEPT);
        ring.push_sqe(&sqe).map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "SQ full")
        })?;
    }
    ring.submit()?;

    eprintln!("[vortex] Worker {} listening on fd {}", core_id, listener_fd);

    let mut cqes = [(0u64, 0i32, 0u32); 512];

    loop {
        date.maybe_update();
        ring.submit_and_wait(1)?;

        let mut cqe_count = 0usize;
        for cqe in ring.completions() {
            if cqe_count < 512 {
                cqes[cqe_count] = (cqe.user_data(), cqe.result(), cqe.flags());
                cqe_count += 1;
            }
        }

        for ci in 0..cqe_count {
            let (user_data, result, flags) = cqes[ci];
            let token_type = user_data >> 32;

            match token_type {
                // ── Accept ──────────────────────────────────────────
                0 => {
                    if result < 0 { continue; }
                    let conn_fd = result;
                    let _ = socket::configure_accepted(conn_fd);

                    let slot = match file_table.alloc() {
                        Some(s) => s,
                        None => { unsafe { libc::close(conn_fd); } continue; }
                    };
                    if registered::update_file(&ring.submitter(), slot, conn_fd).is_err() {
                        file_table.free(slot);
                        unsafe { libc::close(conn_fd); }
                        continue;
                    }
                    unsafe { libc::close(conn_fd); }

                    let si = slot as usize;
                    if si >= connections.len() {
                        connections.resize_with(si + 1024, || None);
                    }
                    connections[si] = Some(Connection {
                        send_buf: send_buf_pool.pop().unwrap_or_else(|| vec![0u8; SEND_BUF_SIZE]),
                    });

                    unsafe {
                        let sqe = multishot::prep_recv_buf_select_fixed(
                            slot, buf_ring.buf_size(), buf_ring.bgid(),
                            TOKEN_RECV_BASE | slot as u64,
                        );
                        let _ = ring.push_sqe(&sqe);
                    }
                }

                // ── HTTP recv ───────────────────────────────────────
                1 => {
                    let slot = (user_data & 0xFFFFFFFF) as u32;
                    let si = slot as usize;

                    if result <= 0 {
                        close_connection(&mut connections, si, slot, &mut ring, &mut send_buf_pool)?;
                        continue;
                    }

                    let len = result as usize;
                    let buf_id = multishot::buffer_id(flags).unwrap();
                    let recv_data = buf_ring.get_buf(buf_id, len);
                    let action = A::classify(recv_data);

                    match action {
                        RouteAction::Fast(id) => {
                            let resp_len = if let Some(conn) = &mut connections[si] {
                                let (_count, rlen) = A::handle_fast(id, recv_data, &mut conn.send_buf, &date);
                                rlen
                            } else { 0 };
                            buf_ring.return_buf(buf_id);

                            if resp_len > 0 {
                                if let Some(conn) = &connections[si] {
                                    unsafe {
                                        let sqe = multishot::prep_send_fixed(
                                            slot, conn.send_buf.as_ptr(), resp_len as u32,
                                            TOKEN_SEND_BASE | slot as u64,
                                        );
                                        let _ = ring.push_sqe(&sqe);
                                    }
                                }
                            } else {
                                close_connection(&mut connections, si, slot, &mut ring, &mut send_buf_pool)?;
                            }
                        }

                        RouteAction::Db { id, queries } => {
                            buf_ring.return_buf(buf_id);

                            if db_conns.is_empty() {
                                let resp_len = write_503(&mut connections, si);
                                if resp_len > 0 {
                                    submit_http_send(slot, &connections, si, resp_len, &mut ring);
                                }
                            } else if let Some(db_idx) = db_conns.iter().position(|c| c.idle) {
                                start_db_op::<A>(&mut db_conns[db_idx], db_idx, slot, id, queries, &mut ring);
                            } else {
                                db_queue.push_back(DbRequest { http_slot: slot, route_id: id, queries });
                            }
                        }

                        RouteAction::NotFound => {
                            buf_ring.return_buf(buf_id);
                            close_connection(&mut connections, si, slot, &mut ring, &mut send_buf_pool)?;
                        }
                    }
                }

                // ── HTTP send complete ──────────────────────────────
                2 => {
                    let slot = (user_data & 0xFFFFFFFF) as u32;
                    let si = slot as usize;

                    if result < 0 {
                        close_connection(&mut connections, si, slot, &mut ring, &mut send_buf_pool)?;
                    } else if connections[si].is_some() {
                        unsafe {
                            let sqe = multishot::prep_recv_buf_select_fixed(
                                slot, buf_ring.buf_size(), buf_ring.bgid(),
                                TOKEN_RECV_BASE | slot as u64,
                            );
                            let _ = ring.push_sqe(&sqe);
                        }
                    }
                }

                // ── Close complete ──────────────────────────────────
                3 => {
                    let slot = (user_data & 0xFFFFFFFF) as u32;
                    file_table.free(slot);
                }

                // ── DB send complete ────────────────────────────────
                4 => {
                    let db_idx = (user_data & 0xFFFFFFFF) as usize;
                    let db = &mut db_conns[db_idx];

                    if result < 0 {
                        let hs = db.http_slot;
                        db.idle = true;
                        let resp_len = write_500(&mut connections, hs as usize);
                        if resp_len > 0 { submit_http_send(hs, &connections, hs as usize, resp_len, &mut ring); }
                        drain_db_queue::<A>(&mut db_queue, &mut db_conns, &connections, &mut ring);
                        continue;
                    }

                    // Submit recv on DB socket
                    unsafe {
                        let sqe = multishot::prep_recv_fixed(
                            db.slot,
                            db.rbuf.as_mut_ptr().add(db.rpos),
                            (db.rbuf.len() - db.rpos) as u32,
                            TOKEN_DB_RECV | db_idx as u64,
                        );
                        let _ = ring.push_sqe(&sqe);
                    }
                }

                // ── DB recv complete ────────────────────────────────
                5 => {
                    let db_idx = (user_data & 0xFFFFFFFF) as usize;
                    let db = &mut db_conns[db_idx];

                    if result <= 0 {
                        let hs = db.http_slot;
                        db.idle = true;
                        let resp_len = write_500(&mut connections, hs as usize);
                        if resp_len > 0 { submit_http_send(hs, &connections, hs as usize, resp_len, &mut ring); }
                        drain_db_queue::<A>(&mut db_queue, &mut db_conns, &connections, &mut ring);
                        continue;
                    }

                    db.rpos += result as usize;

                    // Check if complete PG response (ReadyForQuery found)
                    let rpos = db.rpos;
                    if vortex_db::wire::try_find_ready(&db.rbuf[..rpos]).is_none() {
                        unsafe {
                            let sqe = multishot::prep_recv_fixed(
                                db.slot,
                                db.rbuf.as_mut_ptr().add(db.rpos),
                                (db.rbuf.len() - db.rpos) as u32,
                                TOKEN_DB_RECV | db_idx as u64,
                            );
                            let _ = ring.push_sqe(&sqe);
                        }
                        continue;
                    }

                    // ── Complete PG response — delegate to App ──
                    let hs = db.http_slot;
                    let hi = hs as usize;

                    let resp_len = if let Some(conn) = &mut connections[hi] {
                        A::db_finish(&mut db.state, &db.rbuf, db.rpos, &mut conn.send_buf, &date, &mut body_buf)
                    } else { 0 };

                    db.idle = true;
                    if resp_len > 0 { submit_http_send(hs, &connections, hi, resp_len, &mut ring); }

                    // DB conn became idle — drain queued requests
                    drain_db_queue::<A>(&mut db_queue, &mut db_conns, &connections, &mut ring);
                }

                _ => {}
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Submit an HTTP send SQE for a connection.
#[inline]
fn submit_http_send(
    slot: u32,
    connections: &[Option<Connection>],
    si: usize,
    resp_len: usize,
    ring: &mut Ring,
) {
    if let Some(conn) = &connections[si] {
        unsafe {
            let sqe = multishot::prep_send_fixed(
                slot, conn.send_buf.as_ptr(), resp_len as u32,
                TOKEN_SEND_BASE | slot as u64,
            );
            let _ = ring.push_sqe(&sqe);
        }
    }
}

/// Build PG wire protocol messages and submit send to DB socket.
fn start_db_op<A: App>(
    db: &mut AsyncDbConn<A::DbState>,
    db_idx: usize,
    http_slot: u32,
    route_id: u8,
    queries: i32,
    ring: &mut Ring,
) {
    db.idle = false;
    db.http_slot = http_slot;
    db.rpos = 0;
    db.wbuf.clear();

    A::db_start(route_id, queries, &mut db.wbuf, &mut db.state);

    unsafe {
        let sqe = multishot::prep_send_fixed(
            db.slot, db.wbuf.as_ptr(), db.wbuf.len() as u32,
            TOKEN_DB_SEND | db_idx as u64,
        );
        let _ = ring.push_sqe(&sqe);
    }
}

/// Process queued DB requests when any connection becomes idle.
fn drain_db_queue<A: App>(
    queue: &mut VecDeque<DbRequest>,
    db_conns: &mut [AsyncDbConn<A::DbState>],
    connections: &[Option<Connection>],
    ring: &mut Ring,
) {
    while let Some(db_idx) = db_conns.iter().position(|c| c.idle) {
        match queue.pop_front() {
            Some(req) if connections[req.http_slot as usize].is_some() => {
                start_db_op::<A>(&mut db_conns[db_idx], db_idx, req.http_slot, req.route_id, req.queries, ring);
            }
            Some(_) => continue, // HTTP connection gone, try next
            None => return,
        }
    }
}

// ── Error responses ─────────────────────────────────────────────────

#[cold]
#[inline(never)]
fn write_500(connections: &mut [Option<Connection>], fd_idx: usize) -> usize {
    const RESP: &[u8] = b"HTTP/1.1 500 Internal Server Error\r\nServer: V\r\nContent-Length: 0\r\n\r\n";
    if let Some(conn) = &mut connections[fd_idx] {
        conn.send_buf[..RESP.len()].copy_from_slice(RESP);
        RESP.len()
    } else {
        0
    }
}

#[cold]
#[inline(never)]
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
    slot_idx: usize,
    slot: u32,
    ring: &mut Ring,
    send_buf_pool: &mut Vec<Vec<u8>>,
) -> io::Result<()> {
    if slot_idx < connections.len() {
        if let Some(conn) = connections[slot_idx].take() {
            send_buf_pool.push(conn.send_buf);
            unsafe {
                let sqe = multishot::prep_close_fixed(slot, TOKEN_CLOSE_BASE | slot as u64);
                let _ = ring.push_sqe(&sqe);
            }
        }
    }
    Ok(())
}
