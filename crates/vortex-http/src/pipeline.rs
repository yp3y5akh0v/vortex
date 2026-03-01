//! HTTP pipelining support.
//!
//! TechEmpower's wrk client pipelines up to 16 requests per connection.
//! We process all requests in a single recv buffer and batch responses.

use crate::parser::{self, Route};
use crate::response::{JsonResponse, NotFoundResponse, PlaintextResponse};
use crate::date::DateCache;

/// Process all pipelined requests in a buffer and write responses.
///
/// Returns (requests_processed, response_bytes_written).
#[inline]
pub fn process_pipelined(
    recv_buf: &[u8],
    send_buf: &mut [u8],
    date: &DateCache,
) -> (usize, usize) {
    let mut recv_offset = 0;
    let mut send_offset = 0;
    let mut count = 0;

    while recv_offset < recv_buf.len() {
        // Find the end of the current request
        let remaining = &recv_buf[recv_offset..];
        let req_end = match parser::find_request_end(remaining) {
            Some(end) => end,
            None => break, // Incomplete request
        };

        // Classify the request using fast-path
        let route = parser::classify_fast(remaining);

        // Write the response
        let written = match route {
            Route::Plaintext => PlaintextResponse::write(&mut send_buf[send_offset..], date),
            Route::Json => JsonResponse::write(&mut send_buf[send_offset..], date),
            _ => NotFoundResponse::write(&mut send_buf[send_offset..]),
        };

        send_offset += written;
        recv_offset += req_end;
        count += 1;
    }

    (count, send_offset)
}
