//! Registered buffers and fixed file descriptors.
//!
//! Pre-registering resources with io_uring avoids per-operation overhead
//! for page pinning (buffers) and fd lookup (file descriptors).

use std::io;

/// Register a set of buffers with the io_uring instance.
///
/// Once registered, operations can reference buffers by index instead
/// of pointer, avoiding page pinning overhead on every operation.
///
/// # Safety
/// The caller must ensure buffer pointers and lengths remain valid
/// until unregistration or ring destruction.
pub unsafe fn register_buffers(
    submitter: &io_uring::Submitter<'_>,
    bufs: &[libc::iovec],
) -> io::Result<()> {
    submitter.register_buffers(bufs)?;
    Ok(())
}

/// Register a set of file descriptors with the io_uring instance.
///
/// Once registered, operations can reference fds by index,
/// avoiding fd table lookup and atomic reference counting per operation.
pub fn register_files(
    submitter: &io_uring::Submitter<'_>,
    fds: &[i32],
) -> io::Result<()> {
    submitter.register_files(fds)?;
    Ok(())
}
