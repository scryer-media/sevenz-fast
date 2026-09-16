//! The one place in this crate that names `lzma_fast`.
//!
//! Upstream `sevenz-rust2` decodes the LZMA (`03 01 01`) and LZMA2 (`21`)
//! coders with `lzma-rust2`. This fork decodes them with
//! [`lzma-fast`](https://github.com/scryer-media/lzma-fast), a port of Igor
//! Pavlov's reference decoder. Everything that swap needs is behind this
//! module so that adopting `lzma-fast`'s multi-threaded LZMA2 API, when it
//! lands, is a change to this file and nothing else. The intended shape of
//! that API is written down in `docs/lzma-fast-requests.md`, and the seam it
//! plugs into is [`Lzma2Plan`] below.

use std::io::Read;

use lzma_fast::{Lzma2Reader, LzmaProps, LzmaReader};

use crate::error::Error;

/// Bytes an LZMA or LZMA2 decoder holds beyond its dictionary: the range
/// decoder's input buffer, the probability tables and the chunk buffer. The
/// reference decoder's own accounting is tens of kilobytes; a megabyte covers
/// it with room, and `crate::limits` documents the whole table.
pub(crate) const LZ_STATE_BYTES: u64 = 1 << 20;

/// Decodes the dictionary size an LZMA coder's five property bytes declare.
///
/// The properties are `lc/lp/pb` packed into byte 0, then the dictionary size
/// as a little-endian `u32`.
pub(crate) fn lzma_dictionary_size(properties: &[u8]) -> Result<u32, Error> {
    if properties.len() < 5 {
        return Err(Error::other("LZMA properties too short"));
    }
    Ok(u32::from_le_bytes([
        properties[1],
        properties[2],
        properties[3],
        properties[4],
    ]))
}

/// Decodes the dictionary size an LZMA2 coder's single property byte declares.
///
/// Values up to 39 map to `(2 | (p & 1)) << (p / 2 + 11)`; 40 is the 4 GiB
/// maximum; anything else, or any reserved bit, is a malformed archive.
pub(crate) fn lzma2_dictionary_size(properties: &[u8]) -> Result<u32, Error> {
    let Some(&bits) = properties.first() else {
        return Err(Error::other("LZMA2 properties too short"));
    };
    let bits = u32::from(bits);
    if (bits & !0x3F) != 0 {
        return Err(Error::other("Unsupported LZMA2 property bits"));
    }
    if bits > 40 {
        return Err(Error::other("Dictionary larger than 4GiB maximum size"));
    }
    if bits == 40 {
        return Ok(0xFFFF_FFFF);
    }
    Ok((2 | (bits & 1)) << (bits / 2 + 11))
}

/// Kilobytes an LZMA2 decode of `dict_size` needs, for the reader's
/// `max_mem_limit_kb` check.
pub(crate) fn lzma2_memory_usage_kb(dict_size: u32) -> usize {
    let bytes = u64::from(dict_size).saturating_add(LZ_STATE_BYTES);
    bytes.div_ceil(1024) as usize
}

/// Builds the LZMA1 decoder for a 7z coder.
///
/// `uncompressed_len` is the coder's declared output size; a 7z LZMA1 stream
/// carries no end marker, so the length is what stops the decode.
pub(crate) fn lzma_decoder<R: Read>(
    input: R,
    uncompressed_len: usize,
    properties: &[u8],
) -> Result<LzmaReader<R>, std::io::Error> {
    // The caller has already rejected a properties field shorter than five
    // bytes; `LzmaProps::parse` wants exactly five.
    let mut raw = [0u8; lzma_fast::LZMA_PROPS_SIZE];
    raw.copy_from_slice(&properties[..lzma_fast::LZMA_PROPS_SIZE]);
    let props = LzmaProps::parse(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    LzmaReader::with_props(input, props, Some(uncompressed_len as u64))
}

/// How the LZMA2 coder of one block should be decoded.
///
/// Today there is one answer. The variant exists so the call site in
/// `decoder.rs` already asks the question, and so the multi-threaded reader
/// slots in here rather than in a `match` somewhere else.
///
/// # The adapter point for multi-threaded LZMA2
///
/// This is the seam, and it is deliberately the whole of it. When
/// `lzma-fast`'s parallel reader lands, this enum grows one variant:
///
/// ```ignore
/// pub(crate) enum Lzma2Plan<S> {
///     SingleThreaded,
///     Parallel {
///         /// The block's packed stream as a seekable view — this crate knows
///         /// its absolute range from `Archive::block_pack_streams`, so the
///         /// parallel decoder can read runs out of order without a second
///         /// open file.
///         source: S,
///         /// Workers to use. Already clamped to 1..=256 by the reader.
///         threads: NonZeroU32,
///         /// Ceiling on what the decode may allocate. A parallel LZMA2
///         /// decode buffers a whole dependent run, so this is not the
///         /// dictionary-based number `Archive::decoder_memory_estimate`
///         /// reports, and it has to be enforced rather than assumed.
///         memory_limit: u64,
///     },
/// }
/// ```
///
/// and [`lzma2_decoder`] grows a sibling that takes `Read + Seek`. Two
/// properties of that design are load-bearing and are stated as requirements
/// in `docs/lzma-fast-requests.md`: the output stays *in order* (the coder
/// above it in the chain is a plain `Read`), and the choice between modes is
/// *lossless and revisable while decoding* — whether a stream can be decoded
/// in parallel depends on how the encoder chunked it, which is not visible in
/// the coder properties, so the reader starts single-threaded and widens at a
/// run boundary rather than failing on a stream that turns out to be one
/// dependent run.
///
/// Nothing else in this crate needs to change for that: `decoder.rs` already
/// asks this question, and the archive-level plumbing that supplies the pack
/// range, the thread count and the memory limit already exists
/// ([`crate::ArchiveLimits`], [`crate::Archive::block_pack_streams`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lzma2Plan {
    /// One dependent stream decoded on the calling thread.
    SingleThreaded,
}

impl Lzma2Plan {
    /// Chooses how to decode, given the caller's thread count.
    ///
    /// A thread count above one currently decodes single-threaded anyway: the
    /// multi-threaded reader this fork wants does not exist yet, and upstream's
    /// (which buffers a whole dependent run before emitting anything) is in
    /// `lzma-rust2`, which this fork does not link at runtime. The thread count
    /// is therefore accepted and ignored rather than rejected, so that callers
    /// and archives keep working unchanged.
    pub(crate) fn for_thread_count(_threads: u32) -> Self {
        Self::SingleThreaded
    }
}

/// Builds the LZMA2 decoder for a 7z coder.
pub(crate) fn lzma2_decoder<R: Read>(
    input: R,
    properties: &[u8],
    plan: Lzma2Plan,
) -> Result<Lzma2Reader<R>, std::io::Error> {
    let dict_prop = properties[0];
    match plan {
        Lzma2Plan::SingleThreaded => Lzma2Reader::new(input, dict_prop),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property byte table, against the values the reference decoder
    /// computes for the ends and a midpoint of the range.
    #[test]
    fn lzma2_property_bytes_decode_like_the_reference() {
        assert_eq!(lzma2_dictionary_size(&[0]).unwrap(), 4096);
        assert_eq!(lzma2_dictionary_size(&[1]).unwrap(), 6144);
        assert_eq!(lzma2_dictionary_size(&[24]).unwrap(), 16 << 20);
        assert_eq!(lzma2_dictionary_size(&[40]).unwrap(), u32::MAX);
        assert!(lzma2_dictionary_size(&[41]).is_err());
        assert!(lzma2_dictionary_size(&[0x40]).is_err());
        assert!(lzma2_dictionary_size(&[]).is_err());
    }

    #[test]
    fn lzma_dictionary_comes_from_the_last_four_property_bytes() {
        let mut props = vec![0x5D];
        props.extend_from_slice(&(8u32 << 20).to_le_bytes());
        assert_eq!(lzma_dictionary_size(&props).unwrap(), 8 << 20);
        assert!(lzma_dictionary_size(&[0x5D, 0, 0]).is_err());
    }
}
