//! vortex-template: HTML template rendering for Vortex.
//!
//! Hand-optimized Fortunes template with HTML entity escaping.

/// Escape HTML entities in a string, writing to the output buffer.
///
/// Escapes: & < > " '
/// Uses a lookup table to bulk-copy safe byte ranges via extend_from_slice.
#[inline]
pub fn escape_html(input: &[u8], output: &mut Vec<u8>) {
    // 0 = safe byte, 1-5 = index into ESCAPES
    static LUT: [u8; 256] = {
        let mut t = [0u8; 256];
        t[b'&' as usize] = 1;
        t[b'<' as usize] = 2;
        t[b'>' as usize] = 3;
        t[b'"' as usize] = 4;
        t[b'\'' as usize] = 5;
        t
    };
    static ESCAPES: [&[u8]; 6] = [b"", b"&amp;", b"&lt;", b"&gt;", b"&quot;", b"&#x27;"];

    let mut last = 0;
    for i in 0..input.len() {
        let idx = LUT[input[i] as usize];
        if idx != 0 {
            if i > last {
                output.extend_from_slice(&input[last..i]);
            }
            output.extend_from_slice(ESCAPES[idx as usize]);
            last = i + 1;
        }
    }
    if last < input.len() {
        output.extend_from_slice(&input[last..]);
    }
}

/// Render the Fortunes HTML template into the provided output buffer.
///
/// Takes raw DB fortunes, adds the extra fortune, sorts, and renders.
/// Clears output before writing.
pub fn render_fortunes(db_fortunes: &[(i32, String)], output: &mut Vec<u8>) {
    // Build full list with the extra fortune
    let mut fortunes: Vec<(i32, &str)> = db_fortunes
        .iter()
        .map(|(id, msg)| (*id, msg.as_str()))
        .collect();
    fortunes.push((0, "Additional fortune added at request time."));

    // Sort by message (TechEmpower requirement)
    fortunes.sort_by(|a, b| a.1.cmp(b.1));

    // Render HTML
    output.clear();
    output.extend_from_slice(HEADER);

    let mut id_buf = itoa::Buffer::new();
    for &(id, message) in &fortunes {
        output.extend_from_slice(b"<tr><td>");
        output.extend_from_slice(id_buf.format(id).as_bytes());
        output.extend_from_slice(b"</td><td>");
        escape_html(message.as_bytes(), output);
        output.extend_from_slice(b"</td></tr>");
    }

    output.extend_from_slice(FOOTER);
}

const HEADER: &[u8] = b"<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table><tr><th>id</th><th>message</th></tr>";
const FOOTER: &[u8] = b"</table></body></html>";
const EXTRA_FORTUNE: &[u8] = b"Additional fortune added at request time.";

/// Render fortunes from zero-copy byte slices. No String or Vec allocation.
/// Uses a fixed-size stack array and insertion sort.
pub fn render_fortunes_zerocopy(db_fortunes: &[(i32, &[u8])], count: usize, output: &mut Vec<u8>) {
    // Stack array: up to 15 DB rows + 1 extra fortune
    let mut fortunes = [(0i32, &b""[..]); 16];
    let n = count.min(15);
    for i in 0..n {
        fortunes[i] = db_fortunes[i];
    }
    fortunes[n] = (0, EXTRA_FORTUNE);
    let total = n + 1;

    // Insertion sort — optimal for N=13, zero allocation
    for i in 1..total {
        let key = fortunes[i];
        let mut j = i;
        while j > 0 && fortunes[j - 1].1 > key.1 {
            fortunes[j] = fortunes[j - 1];
            j -= 1;
        }
        fortunes[j] = key;
    }

    // Render HTML
    output.clear();
    output.extend_from_slice(HEADER);

    let mut id_buf = itoa::Buffer::new();
    for i in 0..total {
        let (id, message) = fortunes[i];
        output.extend_from_slice(b"<tr><td>");
        output.extend_from_slice(id_buf.format(id).as_bytes());
        output.extend_from_slice(b"</td><td>");
        escape_html(message, output);
        output.extend_from_slice(b"</td></tr>");
    }

    output.extend_from_slice(FOOTER);
}
