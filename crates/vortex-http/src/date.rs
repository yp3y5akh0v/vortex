//! Cached HTTP Date header.
//!
//! The Date header changes once per second. We cache it per-thread
//! and update via a 1-second check in the event loop.
//! Format: "Date: Thu, 01 Jan 1970 00:00:00 GMT\r\n"

/// Cached Date header value.
///
/// Updated once per second by the worker's event loop timer.
pub struct DateCache {
    /// The complete "Date: ...\r\n" header bytes.
    buf: [u8; 37],
    /// Unix timestamp of last update.
    last_update: i64,
}

impl DateCache {
    /// Date header length including "Date: " prefix and "\r\n" suffix.
    const HEADER_LEN: usize = 37;

    pub fn new() -> Self {
        let mut cache = Self {
            buf: [0u8; 37],
            last_update: 0,
        };
        cache.update();
        cache
    }

    /// Update the cached date if the second has changed.
    #[inline]
    pub fn maybe_update(&mut self) {
        let now = unix_time();
        if now != self.last_update {
            self.last_update = now;
            self.update();
        }
    }

    /// Force update the cached date header — zero-allocation.
    fn update(&mut self) {
        let now = unix_time();
        self.last_update = now;

        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::gmtime_r(&now, &mut tm);

            const DAYS: [&[u8; 3]; 7] = [b"Sun", b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat"];
            const MONTHS: [&[u8; 3]; 12] = [
                b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun",
                b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
            ];

            let b = &mut self.buf;
            // "Date: "
            b[0] = b'D'; b[1] = b'a'; b[2] = b't'; b[3] = b'e'; b[4] = b':'; b[5] = b' ';
            // "Thu"
            let day = DAYS[tm.tm_wday as usize];
            b[6] = day[0]; b[7] = day[1]; b[8] = day[2];
            // ", "
            b[9] = b','; b[10] = b' ';
            // "01"
            b[11] = b'0' + (tm.tm_mday / 10) as u8;
            b[12] = b'0' + (tm.tm_mday % 10) as u8;
            // " "
            b[13] = b' ';
            // "Jan"
            let mon = MONTHS[tm.tm_mon as usize];
            b[14] = mon[0]; b[15] = mon[1]; b[16] = mon[2];
            // " "
            b[17] = b' ';
            // "1970"
            let year = (tm.tm_year + 1900) as u32;
            b[18] = b'0' + (year / 1000) as u8;
            b[19] = b'0' + ((year / 100) % 10) as u8;
            b[20] = b'0' + ((year / 10) % 10) as u8;
            b[21] = b'0' + (year % 10) as u8;
            // " "
            b[22] = b' ';
            // "00:00:00"
            b[23] = b'0' + (tm.tm_hour / 10) as u8;
            b[24] = b'0' + (tm.tm_hour % 10) as u8;
            b[25] = b':';
            b[26] = b'0' + (tm.tm_min / 10) as u8;
            b[27] = b'0' + (tm.tm_min % 10) as u8;
            b[28] = b':';
            b[29] = b'0' + (tm.tm_sec / 10) as u8;
            b[30] = b'0' + (tm.tm_sec % 10) as u8;
            // " GMT\r\n"
            b[31] = b' '; b[32] = b'G'; b[33] = b'M'; b[34] = b'T';
            b[35] = b'\r'; b[36] = b'\n';
        }
    }

    /// Get the complete Date header bytes.
    #[inline(always)]
    pub fn header_bytes(&self) -> &[u8] {
        &self.buf[..Self::HEADER_LEN]
    }
}

impl Default for DateCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Get current Unix timestamp.
#[inline]
fn unix_time() -> i64 {
    unsafe {
        let mut tv: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_REALTIME, &mut tv);
        tv.tv_sec
    }
}
