//! PGO profiling harness for Vortex hot paths.
//!
//! Exercises HTTP parsing, response generation, JSON serialization,
//! and template rendering to generate LLVM PGO profile data.
//! Does NOT require io_uring or network access — safe to run during
//! `docker build`.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::hint::black_box;
use vortex_http::date::DateCache;
use vortex_http::parser;
use vortex_http::pipeline;
use vortex_http::response::{DynHtmlResponse, DynJsonResponse};

fn main() {
    let mut date = DateCache::new();

    // Realistic HTTP requests matching TFB wrk format
    let plaintext_req =
        b"GET /plaintext HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";
    let json_req =
        b"GET /json HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";
    let queries_req =
        b"GET /queries?q=20 HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";
    let updates_req =
        b"GET /updates?q=20 HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";
    let fortunes_req =
        b"GET /fortunes HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";
    let db_req =
        b"GET /db HTTP/1.1\r\nHost: tfb-server:8080\r\nAccept: */*\r\n\r\n";

    // 16x pipelined plaintext (TechEmpower methodology)
    let mut pipelined = Vec::new();
    for _ in 0..16 {
        pipelined.extend_from_slice(plaintext_req);
    }

    let mut send_buf = vec![0u8; 65536];
    let mut body_buf = vec![0u8; 32768];
    let mut html_buf = Vec::with_capacity(4096);

    let fortunes: Vec<(i32, String)> = vec![
        (1, "a1b2c3".into()),
        (2, "f8e7d6c5".into()),
        (3, "<b>3a9f</b>".into()),
        (4, "7c4e2f8a".into()),
        (5, "0".into()),
        (6, "4f2a8b".into()),
        (7, "c9d3e1f7a2b84c06".into()),
        (8, "9d3e".into()),
        (9, "1a2b3c4d5e".into()),
        (10, "ff00".into()),
        (11, "b7".into()),
        (12, "8a3c6f2e91".into()),
        (13, "5e7f42".into()),
        (14, "3b6a9d1c".into()),
        (15, "7e9f2d".into()),
    ];

    // Exercise hot paths proportional to real TFB workload:
    // - Plaintext pipelined: highest volume
    // - JSON: high volume
    // - DB/queries/updates JSON: medium volume
    // - Fortunes HTML: lower volume
    for i in 0..500_000u32 {
        date.maybe_update();

        // Route classification (runs on every recv)
        black_box(parser::classify_fast(plaintext_req));
        black_box(parser::classify_fast(json_req));
        black_box(parser::classify_fast(db_req));
        black_box(parser::classify_fast(queries_req));
        black_box(parser::classify_fast(fortunes_req));
        black_box(parser::classify_fast(updates_req));

        // Query parameter parsing
        black_box(parser::parse_queries_param(queries_req));
        black_box(parser::parse_queries_param(updates_req));

        // Request boundary detection
        black_box(parser::find_request_end(plaintext_req));
        black_box(parser::count_request_boundaries(&pipelined));

        // Pipelined response generation (primary plaintext hot path)
        black_box(pipeline::process_pipelined(
            &pipelined,
            &mut send_buf,
            &date,
        ));

        // Single JSON response
        black_box(pipeline::process_pipelined(
            json_req.as_slice(),
            &mut send_buf,
            &date,
        ));

        // Dynamic JSON: single world (/db endpoint)
        let id = (i % 10000 + 1) as i32;
        let rn = (i.wrapping_mul(7).wrapping_add(3) % 10000 + 1) as i32;
        let wlen = vortex_json::write_world(&mut body_buf, id, rn);
        black_box(DynJsonResponse::write(
            &mut send_buf,
            &date,
            &body_buf[..wlen],
        ));

        // Multi-world JSON: 20 worlds (/queries, /updates endpoints)
        if i % 10 == 0 {
            let worlds: Vec<(i32, i32)> = (0..20)
                .map(|j| {
                    (
                        (i as i32 + j) % 10000 + 1,
                        (i as i32 * 7 + j) % 10000 + 1,
                    )
                })
                .collect();
            let wlen = vortex_json::write_worlds(&mut body_buf, &worlds);
            black_box(DynJsonResponse::write(
                &mut send_buf,
                &date,
                &body_buf[..wlen],
            ));
        }

        // Fortunes template rendering (/fortunes endpoint)
        if i % 50 == 0 {
            vortex_template::render_fortunes(&fortunes, &mut html_buf);
            black_box(DynHtmlResponse::write(
                &mut send_buf,
                &date,
                &html_buf,
            ));
        }
    }

    eprintln!("[profgen] PGO profile data generated");
}
