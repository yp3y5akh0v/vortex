//! Provided buffer rings for zero-allocation recv.
//!
//! The kernel selects a buffer from the ring when a multishot recv
//! completion is ready. The app processes data and returns the buffer.

use std::io;
use std::ptr;

/// A ring-mapped provided buffer pool.
///
/// Each worker thread maintains one of these. Buffers are used by
/// multishot recv operations without any per-request allocation.
pub struct ProvidedBufRing {
    /// Base pointer to the mmap'd buffer ring memory.
    ring_ptr: *mut u8,
    /// Pointer to the individual buffers.
    bufs_ptr: *mut u8,
    /// Number of buffers in the ring.
    buf_count: u16,
    /// Size of each buffer in bytes.
    buf_size: u32,
    /// Buffer group ID (unique per worker thread).
    bgid: u16,
    /// Current tail position for adding buffers back.
    tail: u16,
}

impl ProvidedBufRing {
    /// Default buffer count per ring.
    pub const DEFAULT_BUF_COUNT: u16 = 2048;
    /// Default buffer size (sufficient for most HTTP/1.1 requests).
    pub const DEFAULT_BUF_SIZE: u32 = 4096;

    /// Create a new provided buffer ring.
    ///
    /// This allocates the buffer memory and prepares it for registration
    /// with io_uring via `io_uring_register_buf_ring`.
    pub fn new(bgid: u16, buf_count: u16, buf_size: u32) -> io::Result<Self> {
        let total_buf_mem = buf_count as usize * buf_size as usize;

        // Allocate aligned buffer memory
        let layout = std::alloc::Layout::from_size_align(total_buf_mem, 4096)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let bufs_ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if bufs_ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "failed to allocate buffer ring memory",
            ));
        }

        Ok(Self {
            ring_ptr: ptr::null_mut(), // Set during registration
            bufs_ptr,
            buf_count,
            buf_size,
            bgid,
            tail: 0,
        })
    }

    /// Get the buffer group ID.
    #[inline]
    pub fn bgid(&self) -> u16 {
        self.bgid
    }

    /// Get a slice to a specific buffer by index.
    #[inline]
    pub fn get_buf(&self, buf_id: u16, len: usize) -> &[u8] {
        debug_assert!((buf_id as u16) < self.buf_count);
        debug_assert!(len <= self.buf_size as usize);
        unsafe {
            let ptr = self.bufs_ptr.add(buf_id as usize * self.buf_size as usize);
            std::slice::from_raw_parts(ptr, len)
        }
    }

    /// Return a buffer to the ring (make it available for kernel reuse).
    #[inline]
    pub fn return_buf(&mut self, buf_id: u16) {
        // In the full implementation, this writes to the ring tail
        // and advances the tail pointer. For now, track the tail.
        self.tail = self.tail.wrapping_add(1);
        let _ = buf_id; // Will be used in full ring implementation
    }

    /// Get the number of buffers.
    #[inline]
    pub fn buf_count(&self) -> u16 {
        self.buf_count
    }

    /// Get the size of each buffer.
    #[inline]
    pub fn buf_size(&self) -> u32 {
        self.buf_size
    }
}

impl Drop for ProvidedBufRing {
    fn drop(&mut self) {
        if !self.bufs_ptr.is_null() {
            let total = self.buf_count as usize * self.buf_size as usize;
            let layout = std::alloc::Layout::from_size_align(total, 4096).unwrap();
            unsafe {
                std::alloc::dealloc(self.bufs_ptr, layout);
            }
        }
    }
}

// Safety: ProvidedBufRing is only accessed from a single worker thread.
unsafe impl Send for ProvidedBufRing {}
