//! Branch/call/jump and delta filters, vendored.
//!
//! # Provenance
//!
//! These files are copied from [`lzma-rust2`] 0.20.1 (`src/filter/`) by Nils
//! Hasenbanck, Apache-2.0, the same licence as this crate. Upstream
//! `sevenz-rust2` reaches them through the `lzma-rust2` dependency; this fork
//! decodes LZMA and LZMA2 with `lzma-fast` instead, and vendoring the filters
//! is what lets `lzma-rust2` leave the runtime dependency graph entirely
//! rather than being carried for three filters.
//!
//! The only changes are mechanical, so a future re-sync stays a diff:
//!
//! - `crate::Read` / `crate::Write` / `crate::Result` become the `std::io`
//!   items they alias there;
//! - the `encoder` feature becomes this crate's `compress`;
//! - `error_invalid_data` and the big-endian `u32` read become the local
//!   helpers below instead of crate-wide ones.
//!
//! Fixes belong upstream in `lzma-rust2` as well as here.
//!
//! [`lzma-rust2`]: https://github.com/hasenbanck/lzma-rust2

// Vendored verbatim, so they carry accessors this crate does not call
// (`into_inner`, `inner`, `inner_mut`). Keeping them keeps a future re-sync a
// diff rather than a merge.
#[allow(dead_code)]
pub(crate) mod bcj;
#[allow(dead_code)]
pub(crate) mod bcj2;
#[allow(dead_code)]
pub(crate) mod delta;

use std::io::{self, Read};

/// `lzma-rust2`'s crate-level helper of the same name.
#[inline(always)]
pub(crate) fn error_invalid_data(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The one method the BCJ2 range decoder needs from `lzma-rust2`'s crate-wide
/// `ByteReader` trait.
pub(crate) trait ByteReaderBe {
    /// Reads a big-endian `u32`.
    fn read_u32_be(&mut self) -> io::Result<u32>;
}

impl<T: Read> ByteReaderBe for T {
    #[inline(always)]
    fn read_u32_be(&mut self) -> io::Result<u32> {
        let mut buf = [0; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }
}
