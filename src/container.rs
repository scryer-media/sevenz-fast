//! Container-level questions a streaming consumer asks before it decodes.
//!
//! Upstream's reader answers "give me the bytes of this entry". A consumer
//! that is feeding the reader from a network download, under a memory budget
//! it has to honour before it allocates, needs to ask a few more things first:
//! how much decoder memory this archive will want, which byte ranges of the
//! file each block lives in, what the per-file checksums are, and — once a
//! block has been decoded and checked — to be told so. This module is where
//! the fork adds those, as read-only accessors over the parsed [`Archive`];
//! nothing here changes how anything decodes.

use crate::archive::{Archive, EncoderMethod};
use crate::block::Coder;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// Limits a caller imposes on an archive *before* the reader allocates for it.
///
/// Both fields default to "no limit", so [`ArchiveLimits::default`] reads like
/// [`ArchiveReader::new`]. The point of the type is the two checks that have to
/// happen before an allocation rather than after one:
///
/// - `max_end_header_bytes` bounds the declared size of the end header, which
///   comes straight out of the first 32 bytes of the file. The reader buffers
///   that many bytes in order to parse it, so an archive from an untrusted
///   source can otherwise name any number it likes.
/// - `memory_limit_bytes` bounds [`Archive::decoder_memory_estimate`], which
///   is dominated by the LZMA/LZMA2 dictionary the block declares — again, a
///   number out of the archive, allocated when the decoder is built.
///
/// [`ArchiveReader::new`]: crate::ArchiveReader::new
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Largest decoder footprint the caller will allow, in bytes.
    pub memory_limit_bytes: u64,
    /// Largest end header the caller will allow to be buffered, in bytes.
    pub max_end_header_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            memory_limit_bytes: u64::MAX,
            max_end_header_bytes: u64::MAX,
        }
    }
}

impl ArchiveLimits {
    /// Both limits at once.
    #[must_use]
    pub fn new(memory_limit_bytes: u64, max_end_header_bytes: u64) -> Self {
        Self {
            memory_limit_bytes,
            max_end_header_bytes,
        }
    }

    /// Only a decoder-memory limit.
    #[must_use]
    pub fn memory(memory_limit_bytes: u64) -> Self {
        Self {
            memory_limit_bytes,
            ..Self::default()
        }
    }

    /// The decoder-memory limit in the kilobytes the internal decoder uses,
    /// saturating rather than wrapping on an unlimited budget.
    pub(crate) fn memory_limit_kb(&self) -> usize {
        usize::try_from(self.memory_limit_bytes.div_ceil(KIB)).unwrap_or(usize::MAX)
    }
}

/// A coder with no memory model, from [`Archive::decoder_memory_estimate`].
///
/// Sizing stops at the first one rather than skipping it: a budget built from
/// a chain with an unknown link in it would be a guess presented as a
/// measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsizedCoder {
    /// The coder's method id, as it appears in the archive.
    pub method_id: Vec<u8>,
    /// Why it could not be sized.
    pub reason: &'static str,
}

impl std::fmt::Display for UnsizedCoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "coder {:02x?}: {}", self.method_id, self.reason)
    }
}

impl std::error::Error for UnsizedCoder {}

/// Where one of a block's packed streams lives in the archive file.
///
/// Offsets are absolute: a caller can hand the range straight to a reader that
/// only knows about byte positions, with no 7z arithmetic of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackStreamRange {
    /// Index of the stream in [`Archive::pack_sizes`].
    ///
    /// [`Archive::pack_sizes`]: crate::Archive::pack_sizes
    pub index: usize,
    /// Absolute offset of the first packed byte in the archive.
    pub offset: u64,
    /// Number of packed bytes.
    pub size: u64,
}

impl PackStreamRange {
    /// Offset one past the last packed byte.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.size)
    }
}

/// One entry's worth of sub-stream metadata, as `SubStreamsInfo` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubStream {
    /// Index into the archive's flat list of sub-streams.
    pub index: usize,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// CRC-32 of the uncompressed bytes, when the archive records one.
    pub crc: Option<u32>,
}

/// A block that finished decoding, reported to a completion hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCompletion {
    /// Index of the block in [`Archive::blocks`].
    ///
    /// [`Archive::blocks`]: crate::Archive::blocks
    pub block_index: usize,
    /// Uncompressed bytes the block produced.
    pub unpacked_size: u64,
    /// Whether a CRC-32 was checked against the header while decoding it. It
    /// is `false` when the archive records no checksum for the block or its
    /// sub-streams, not when a check failed — a failed check is an error, and
    /// the hook is never reached.
    pub crc_verified: bool,
}

// ---------------------------------------------------------------------------
// Decoder memory model
// ---------------------------------------------------------------------------

/// Range decoder input buffer plus the LZMA state tables. The LZMA2 reader's
/// own accounting names 40 KiB of state and a 64 KiB compressed-chunk buffer;
/// LZMA's is the same order. Rounded up to a full megabyte.
const LZ_STATE_BYTES: u64 = MIB;
/// The PPMd model is one allocation of exactly the declared size; this covers
/// the decoder's own tables around it.
const PPMD_STATE_BYTES: u64 = MIB;
/// A bzip2 block is at most 900 KiB, and the decoder holds a few times that.
const BZIP2_BYTES: u64 = 8 * MIB;
/// Deflate's window is 32 KiB and the reader wraps its input in a buffer.
const DEFLATE_BYTES: u64 = MIB;
/// Brotli's largest standard window is 16 MiB.
const BROTLI_BYTES: u64 = 32 * MIB;
/// The zstd decoder refuses frames whose window exceeds 128 MiB unless told
/// otherwise, and nothing here tells it otherwise.
const ZSTD_BYTES: u64 = 160 * MIB;
/// An LZ4 frame block is at most 4 MiB, plus a 64 KiB dictionary.
const LZ4_BYTES: u64 = 16 * MIB;
/// Branch/call/jump filters and the delta filter keep a few hundred bytes of
/// state; AES keeps a block. One megabyte covers any of them with room.
const FILTER_BYTES: u64 = MIB;
/// BCJ2 reads four streams at once and keeps a range coder over one of them.
/// Its sub-streams' own decoders are separate coders in the same block and are
/// summed with it.
const BCJ2_BYTES: u64 = 16 * MIB;

impl Archive {
    /// Bytes a single-threaded decode of this archive needs for its decoders.
    ///
    /// Blocks decode one after another, so the answer is the most expensive
    /// block, not the sum of them; within a block the coders are nested
    /// readers that are all live at once, so a block costs the sum of its
    /// chain. Nothing here is exact to the byte: each coder contributes its
    /// dominant allocation — the dictionary, the PPMd model — plus a margin
    /// that covers its state and buffers.
    ///
    /// | Coder | Estimate |
    /// | --- | --- |
    /// | Copy | 0 |
    /// | LZMA, LZMA2 | declared dictionary + 1 MiB |
    /// | PPMd | declared model size + 1 MiB |
    /// | BZip2 | 8 MiB |
    /// | Deflate | 1 MiB |
    /// | Brotli | 32 MiB |
    /// | Zstd | 160 MiB |
    /// | LZ4 | 16 MiB |
    /// | BCJ2 | 16 MiB |
    /// | BCJ, delta, AES-256 | 1 MiB |
    ///
    /// The model is for the **single-threaded** decoders, which is what this
    /// crate currently uses for every coder. A multi-threaded LZMA2 reader
    /// buffers a whole run of dependent chunks before decoding any of it, so
    /// its footprint scales with the block rather than with the dictionary,
    /// and this estimate would not describe it.
    ///
    /// # Errors
    ///
    /// [`UnsizedCoder`] for the first coder with no model, including one whose
    /// properties are too short to read a size out of.
    pub fn decoder_memory_estimate(&self) -> Result<u64, UnsizedCoder> {
        let mut largest_block = 0u64;
        for block in &self.blocks {
            let mut chain = 0u64;
            for coder in &block.coders {
                chain = chain.saturating_add(coder_memory_estimate(coder)?);
            }
            largest_block = largest_block.max(chain);
        }
        Ok(largest_block)
    }
}

/// Bytes one coder's decoder needs.
///
/// # Errors
///
/// [`UnsizedCoder`] if there is no model for this method id.
pub fn coder_memory_estimate(coder: &Coder) -> Result<u64, UnsizedCoder> {
    let method_id = coder.encoder_method_id();
    let properties = coder.properties();

    if method_id == EncoderMethod::ID_COPY {
        Ok(0)
    } else if method_id == EncoderMethod::ID_LZMA {
        Ok(lzma_dictionary_bytes(method_id, properties)?.saturating_add(LZ_STATE_BYTES))
    } else if method_id == EncoderMethod::ID_LZMA2 {
        Ok(lzma2_dictionary_bytes(method_id, properties)?.saturating_add(LZ_STATE_BYTES))
    } else if method_id == EncoderMethod::ID_PPMD {
        Ok(ppmd_model_bytes(method_id, properties)?.saturating_add(PPMD_STATE_BYTES))
    } else if method_id == EncoderMethod::ID_BZIP2 {
        Ok(BZIP2_BYTES)
    } else if method_id == EncoderMethod::ID_DEFLATE {
        Ok(DEFLATE_BYTES)
    } else if method_id == EncoderMethod::ID_BROTLI {
        Ok(BROTLI_BYTES)
    } else if method_id == EncoderMethod::ID_ZSTD {
        Ok(ZSTD_BYTES)
    } else if method_id == EncoderMethod::ID_LZ4 {
        Ok(LZ4_BYTES)
    } else if method_id == EncoderMethod::ID_BCJ2 {
        Ok(BCJ2_BYTES)
    } else if method_id == EncoderMethod::ID_AES256_SHA256
        || method_id == EncoderMethod::ID_DELTA
        || method_id == EncoderMethod::ID_BCJ_X86
        || method_id == EncoderMethod::ID_BCJ_ARM
        || method_id == EncoderMethod::ID_BCJ_ARM64
        || method_id == EncoderMethod::ID_BCJ_ARM_THUMB
        || method_id == EncoderMethod::ID_BCJ_PPC
        || method_id == EncoderMethod::ID_BCJ_IA64
        || method_id == EncoderMethod::ID_BCJ_SPARC
        || method_id == EncoderMethod::ID_BCJ_RISCV
    {
        Ok(FILTER_BYTES)
    } else {
        Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "no memory model for this coder",
        })
    }
}

/// LZMA properties are five bytes: lc/lp/pb, then the dictionary size as a
/// little-endian `u32`.
fn lzma_dictionary_bytes(method_id: &[u8], properties: &[u8]) -> Result<u64, UnsizedCoder> {
    if properties.len() < 5 {
        return Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "LZMA properties shorter than five bytes",
        });
    }
    let dict = u32::from_le_bytes([properties[1], properties[2], properties[3], properties[4]]);
    Ok(u64::from(dict))
}

/// LZMA2 properties are one byte encoding the dictionary size: values up to 39
/// map to `(2 | (p & 1)) << (p / 2 + 11)`, and 40 means the 4 GiB maximum. The
/// decoder rounds the dictionary up to a multiple of sixteen before allocating
/// it, so this does too.
fn lzma2_dictionary_bytes(method_id: &[u8], properties: &[u8]) -> Result<u64, UnsizedCoder> {
    let Some(&bits) = properties.first() else {
        return Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "LZMA2 properties empty",
        });
    };
    let bits = u64::from(bits);
    if bits & !0x3F != 0 {
        return Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "LZMA2 property byte has reserved bits set",
        });
    }
    if bits > 40 {
        return Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "LZMA2 dictionary larger than the 4 GiB maximum",
        });
    }
    let dict = if bits == 40 {
        u64::from(u32::MAX)
    } else {
        (2 | (bits & 1)) << (bits / 2 + 11)
    };
    Ok((dict + 15) & !15)
}

/// PPMd properties are five bytes: the model order, then the model memory size
/// as a little-endian `u32`. The decoder allocates exactly that.
fn ppmd_model_bytes(method_id: &[u8], properties: &[u8]) -> Result<u64, UnsizedCoder> {
    if properties.len() < 5 {
        return Err(UnsizedCoder {
            method_id: method_id.to_vec(),
            reason: "PPMd properties shorter than five bytes",
        });
    }
    let memory = u32::from_le_bytes([properties[1], properties[2], properties[3], properties[4]]);
    Ok(u64::from(memory))
}

// ---------------------------------------------------------------------------
// Read-only views a streaming consumer needs
// ---------------------------------------------------------------------------

impl Archive {
    /// Total number of sub-streams across every block.
    ///
    /// A sub-stream is one entry's worth of a block's output. In a non-solid
    /// archive every block has exactly one; in a solid one a block holds many,
    /// and the per-entry sizes and checksums live in `SubStreamsInfo` rather
    /// than on the block.
    #[must_use]
    pub fn num_unpack_sub_streams(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.num_unpack_sub_streams)
            .sum()
    }

    /// The sub-stream at `index` in the archive's flat sub-stream order.
    ///
    /// Returns `None` past the end. The CRC is `None` when the archive records
    /// none for that sub-stream, which is legal and not a defect.
    #[must_use]
    pub fn sub_stream(&self, index: usize) -> Option<SubStream> {
        let info = self.sub_streams_info.as_ref()?;
        let size = *info.unpack_sizes.get(index)?;
        Some(SubStream {
            index,
            size,
            crc: info
                .has_crc
                .contains(index)
                .then(|| info.crcs.get(index).map(|crc| *crc as u32))
                .flatten(),
        })
    }

    /// Every sub-stream of one block, in order.
    ///
    /// Empty when `block_index` is out of range, or when the archive carries
    /// no `SubStreamsInfo` at all (an archive of one entry per block need not).
    #[must_use]
    pub fn block_sub_streams(&self, block_index: usize) -> Vec<SubStream> {
        let Some(block) = self.blocks.get(block_index) else {
            return Vec::new();
        };
        let Some(&first) = self
            .stream_map
            .block_first_sub_stream_index
            .get(block_index)
        else {
            return Vec::new();
        };
        (first..first + block.num_unpack_sub_streams)
            .filter_map(|index| self.sub_stream(index))
            .collect()
    }

    /// Absolute byte ranges of a block's packed streams, in the order the
    /// block binds them.
    ///
    /// Most blocks have exactly one; a BCJ2 block has four. Empty when
    /// `block_index` is out of range. Offsets are from the start of the
    /// archive file, so a caller can prefetch or bound a read without doing
    /// any 7z arithmetic of its own.
    #[must_use]
    pub fn block_pack_streams(&self, block_index: usize) -> Vec<PackStreamRange> {
        let Some(block) = self.blocks.get(block_index) else {
            return Vec::new();
        };
        let Some(&first) = self
            .stream_map
            .block_first_pack_stream_index
            .get(block_index)
        else {
            return Vec::new();
        };
        let count = block.packed_streams.len().max(1);
        (first..first + count)
            .filter_map(|index| {
                let offset = *self.stream_map.pack_stream_offsets.get(index)?;
                let size = *self.pack_sizes.get(index)?;
                Some(PackStreamRange {
                    index,
                    offset: crate::archive::SIGNATURE_HEADER_SIZE
                        .saturating_add(self.pack_pos)
                        .saturating_add(offset),
                    size,
                })
            })
            .collect()
    }

    /// The coder chain of one block, in the archive's own order.
    ///
    /// Same slice as `archive.blocks[i].coders`, as a checked lookup.
    #[must_use]
    pub fn block_coders(&self, block_index: usize) -> &[Coder] {
        self.blocks
            .get(block_index)
            .map_or(&[], |block| block.coders.as_slice())
    }
}
