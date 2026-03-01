//! Zero-copy send support via IORING_OP_SEND_ZC.
//!
//! Transmits data directly from user memory to NIC without CPU copies.
//! Produces two CQEs: one for send completion, one for buffer release.

use io_uring::opcode;
use io_uring::types::Fd;
use std::os::fd::RawFd;

/// Prepare a zero-copy send SQE.
///
/// Two CQEs will be produced:
/// 1. Send completed (data delivered to socket buffer)
/// 2. Buffer notification (safe to reuse the buffer)
#[inline]
pub fn prep_send_zc(
    conn_fd: RawFd,
    buf: *const u8,
    len: u32,
    user_data: u64,
) -> io_uring::squeue::Entry {
    opcode::SendZc::new(Fd(conn_fd), buf, len)
        .build()
        .user_data(user_data)
}
