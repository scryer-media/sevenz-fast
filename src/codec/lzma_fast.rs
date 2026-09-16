//! The one place in this crate that names `lzma_fast`.
//!
//! Upstream `sevenz-rust2` decodes the LZMA (`03 01 01`) and LZMA2 (`21`)
//! coders with `lzma-rust2`. This fork decodes them with
//! [`lzma-fast`](https://github.com/scryer-media/lzma-fast), a port of Igor
//! Pavlov's reference decoder. Everything that swap needs is behind this
//! module: adopting `lzma-fast`'s multi-threaded LZMA2 decoder was a change to
//! this file and to how the reader hands it a thread count, and to nothing
//! else. What is still asked of that crate is in `docs/lzma-fast-requests.md`;
//! the seam is [`Lzma2Plan`] below.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lzma_fast::crc::CrcFolder;
use lzma_fast::{
    Checksum, ChecksumPlan, DrainStatus, Lzma2AdaptiveDecoder, Lzma2MtOptions, Lzma2Reader,
    LzmaProps, LzmaReader,
};

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
/// Bytes of input read from the coder below in one go when the adaptive
/// decoder asks for more. `lzma-fast`'s own single-threaded path reads in
/// 1 MiB pieces (`IN_BUF_SIZE_ST`); matching it keeps the feed loop's
/// bookkeeping off the profile.
const MT_INPUT_CHUNK: usize = 1 << 20;

/// Smallest in-flight budget a parallel LZMA2 decode is given. Below this the
/// coder decodes single-threaded instead: see [`Lzma2Plan::for_block`].
const MT_MIN_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

/// In-flight budget per thread when the caller set no memory limit at all.
const MT_BUDGET_PER_THREAD_BYTES: u64 = 256 * 1024 * 1024;

/// How much packed input to get ahead by, per thread, before decoding what has
/// been fed. One run per thread is the point of diminishing returns: a worker
/// takes a run only once it has arrived whole, and a run that no idle worker
/// is waiting for is memory spent for nothing. 128 MiB is the run size
/// `7zz -mmt=on` writes, and packed runs are smaller than that.
const MT_FEED_PER_THREAD_BYTES: u64 = 128 * 1024 * 1024;

/// How much may be read ahead without a single worker having been spawned
/// before the reader concludes that reading ahead is buying nothing.
///
/// A stream with no dictionary resets — what `7zz -mmt=1` writes — is one run
/// from beginning to end, and a run is dispatched only once it has arrived
/// whole, so reading ahead on such a stream buffers the entire archive to
/// hand it to a single worker at the end. That is slower than decoding it as
/// it arrives, and holds the whole archive in memory to be so. Past this
/// point the reader stops getting ahead and lets the decoder stream, and it
/// starts again the moment a worker does appear.
const MT_NO_WORKER_GIVE_UP_BYTES: u64 = 256 * 1024 * 1024;

/// The live link between an [`ArchiveReader`] and the LZMA2 coder that is
/// decoding one of its blocks right now.
///
/// Weaver drives an adaptive chase: it decodes with one thread while it is
/// following the tail of a download and widens to several once a backlog of
/// complete runs has built up, then narrows again. Both directions have to
/// work *during* a block, so the thread count cannot be a constructor
/// argument that the coder copies once.
///
/// It is a handful of atomics rather than a lock because the reader writes
/// the thread count from the caller's thread while the coder reads it from
/// the same thread between blocks of output, and the statistics go the other
/// way. Nothing here is a synchronisation point for the decode itself: the
/// coder applies a new thread count at the next run boundary, which is the
/// only place where changing it is lossless.
///
/// [`ArchiveReader`]: crate::ArchiveReader
#[derive(Debug)]
pub(crate) struct Lzma2Control {
    threads: AtomicU32,
    engaged: AtomicBool,
    block_index: AtomicUsize,
    pending_runs: AtomicUsize,
    runs_claimed: AtomicU64,
    in_flight_bytes: AtomicU64,
    spawned_threads: AtomicU32,
    /// Checksums the workers computed, keyed by where in the block's decoded
    /// stream they came from. Empty unless the coder was built with split
    /// points; see [`Lzma2Control::folded`].
    folder: Mutex<CrcFolder<u32>>,
}

impl Lzma2Control {
    pub(crate) fn new(threads: u32) -> Self {
        Self {
            threads: AtomicU32::new(threads.max(1)),
            engaged: AtomicBool::new(false),
            block_index: AtomicUsize::new(0),
            pending_runs: AtomicUsize::new(0),
            runs_claimed: AtomicU64::new(0),
            in_flight_bytes: AtomicU64::new(0),
            spawned_threads: AtomicU32::new(0),
            folder: Mutex::new(CrcFolder::new()),
        }
    }

    /// Adds what a worker computed. Called from the decoding thread, as blocks
    /// are delivered; the pieces arrive in whatever order the workers finished
    /// and the folder sorts them out.
    fn fold_segments(&self, segments: &[lzma_fast::Segment]) {
        if segments.is_empty() {
            return;
        }
        let Ok(mut folder) = self.folder.lock() else {
            return;
        };
        for segment in segments {
            if let Some(crc32) = segment.check.crc32() {
                folder.push(segment.offset, segment.len, crc32);
            }
        }
    }

    /// The CRC-32 of `[offset, offset + len)` of the block's decoded stream,
    /// folded from the worker-computed pieces, or `None` when the pieces do
    /// not cover that range — because the coder was not the parallel one,
    /// because no split points were given, or because the caller has not read
    /// that far.
    pub(crate) fn folded(&self, offset: u64, len: u64) -> Option<u32> {
        self.folder.lock().ok()?.range(offset, len)
    }

    /// Drops the pieces of the block just finished. A new block starts its own
    /// stream at offset zero, so keeping the old ones would let a stale piece
    /// answer for a new range.
    pub(crate) fn clear_folded(&self) {
        if let Ok(mut folder) = self.folder.lock() {
            folder.clear();
        }
    }

    /// Sets the ceiling the next run boundary will use.
    pub(crate) fn set_threads(&self, threads: u32) {
        self.threads.store(threads.max(1), Ordering::Relaxed);
    }

    pub(crate) fn threads(&self) -> u32 {
        self.threads.load(Ordering::Relaxed)
    }

    /// Called by the coder as it is built.
    fn engage(&self) {
        self.engaged.store(true, Ordering::Relaxed);
    }

    /// Called by the coder when its block is done, or has failed. What it was
    /// holding is gone, but what it did — the runs it claimed and the threads
    /// it spawned — stays readable until the next block starts: a caller that
    /// decodes a whole block in one `read` would otherwise have no moment at
    /// which it could see anything at all.
    fn finish(&self) {
        self.pending_runs.store(0, Ordering::Relaxed);
        self.in_flight_bytes.store(0, Ordering::Relaxed);
    }

    /// Called as each block starts, before its coder exists. Everything the
    /// last block reported stops being true here.
    pub(crate) fn set_block_index(&self, block_index: usize) {
        self.block_index.store(block_index, Ordering::Relaxed);
        self.engaged.store(false, Ordering::Relaxed);
        self.pending_runs.store(0, Ordering::Relaxed);
        self.runs_claimed.store(0, Ordering::Relaxed);
        self.in_flight_bytes.store(0, Ordering::Relaxed);
        self.spawned_threads.store(0, Ordering::Relaxed);
        self.clear_folded();
    }

    /// What the coder currently decoding sees, or `None` when no LZMA2 coder
    /// with a parallel plan is live.
    pub(crate) fn progress(&self) -> Option<Lzma2Progress> {
        if !self.engaged.load(Ordering::Relaxed) {
            return None;
        }
        Some(Lzma2Progress {
            block_index: self.block_index.load(Ordering::Relaxed),
            threads: self.threads.load(Ordering::Relaxed),
            spawned_threads: self.spawned_threads.load(Ordering::Relaxed),
            pending_runs: self.pending_runs.load(Ordering::Relaxed),
            runs_claimed: self.runs_claimed.load(Ordering::Relaxed),
            in_flight_bytes: self.in_flight_bytes.load(Ordering::Relaxed),
        })
    }
}

/// A live handle on the LZMA2 coder of whichever block is being decoded.
///
/// This is how a consumer drives an adaptive chase. It is an owned handle
/// rather than a method on the reader because a decode borrows the reader for
/// its duration: the handle is taken first, and then used — from the decoding
/// thread between reads, or from another thread entirely — while the decode
/// runs.
///
/// A handle whose reader is decoding a block whose coder is not LZMA2, or
/// whose LZMA2 coder was built single-threaded, reports
/// [`progress`](Lzma2Handle::progress) as `None` and its
/// [`set_threads`](Lzma2Handle::set_threads) applies to the next block that
/// can use it.
#[derive(Debug, Clone)]
pub struct Lzma2Handle {
    pub(crate) control: Arc<Lzma2Control>,
}

impl Lzma2Handle {
    /// Sets the thread ceiling, effective at the next run boundary. One means
    /// the next run decodes inline on the calling thread.
    ///
    /// The value is clamped to `1..=256`, as on the reader.
    pub fn set_threads(&self, threads: u32) {
        self.control.set_threads(threads.clamp(1, 256));
    }

    /// The ceiling currently in force.
    #[must_use]
    pub fn threads(&self) -> u32 {
        self.control.threads()
    }

    /// What the LZMA2 coder of the block being decoded is doing right now, or
    /// `None` when no block is decoding through the adaptive path.
    #[must_use]
    pub fn progress(&self) -> Option<Lzma2Progress> {
        self.control.progress()
    }
}

/// What the LZMA2 coder of the block being decoded is doing right now.
///
/// A *run* is a piece of the LZMA2 stream that begins with a dictionary reset
/// and is therefore decodable on its own. The count of complete runs that have
/// arrived and have not yet been claimed by a decoder is the backlog an
/// adaptive caller widens on: while it is zero the stream is being chased and
/// there is nothing to parallelise; while it grows there is work that more
/// threads would finish sooner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lzma2Progress {
    /// The block whose LZMA2 coder this describes.
    pub block_index: usize,
    /// Thread ceiling currently in force. One means the next run is decoded
    /// inline on the calling thread.
    pub threads: u32,
    /// Worker threads that exist. Zero until a run is actually dispatched, so
    /// a decoder that never widens never creates one.
    pub spawned_threads: u32,
    /// Complete runs that have arrived and not yet been claimed: the backlog.
    pub pending_runs: usize,
    /// Runs handed to a decoder so far, by either path — the run index of the
    /// current block.
    pub runs_claimed: u64,
    /// Bytes the decoder is holding: buffered input, runs being decoded, and
    /// decoded output not yet handed to the caller.
    pub in_flight_bytes: u64,
}

/// How the LZMA2 coder of one block should be decoded.
///
/// This is the seam, and it is deliberately the whole of it: `decoder.rs` asks
/// the question here and nowhere else.
///
/// # Why the adaptive decoder and not the ring
///
/// `lzma-fast` has two multi-threaded LZMA2 drivers. `Lzma2ParallelDecoder` is
/// the faithful port of 7-Zip's `Lzma2DecMt.c` over `MtDec.c`: it pulls from a
/// `Read` and owns the threads for the whole call, which is the fastest way to
/// decode an archive that is already on disk. Its `Read` adapter spawns the
/// ring behind a thread, so the reader it is given must be `Send + 'static` —
/// and the input of a 7z coder is a bounded view of the caller's archive
/// source, which is neither.
///
/// `Lzma2AdaptiveDecoder` is fed bytes and polled for output, so it borrows
/// nothing: the work it hands to threads is owned copies of complete runs. It
/// is also the only one of the two that can change its mind mid-stream, which
/// is what a consumer chasing a download needs, and what
/// [`Lzma2Control`] exposes here. Both drivers find runs with the same
/// scanner and decode them with the same decoder, so the bytes are identical
/// either way.
pub(crate) enum Lzma2Plan {
    /// One dependent stream decoded on the calling thread, exactly as before:
    /// no control block, no worker, no extra allocation. This is what a caller
    /// that asks for nothing gets.
    SingleThreaded,
    /// The adaptive decoder, with a live thread ceiling and a ceiling on what
    /// it may hold in flight.
    Adaptive {
        /// Workers to use at the first run boundary. Already clamped to
        /// 1..=256 by the reader; one means "capable but inline".
        threads: u32,
        /// Ceiling on what the parallel decode may hold: buffered input, runs
        /// being decoded, and decoded output not yet read. This is *not* the
        /// dictionary-based number `Archive::decoder_memory_estimate` reports,
        /// and it is enforced by the decoder rather than assumed.
        memory_limit: u64,
        /// The live link back to the reader.
        control: Arc<Lzma2Control>,
        /// Where the consumer's boundaries fall in this block's decoded
        /// stream, so each worker checksums the pieces of its own output as
        /// it produces them. Empty when the caller has no boundaries to
        /// declare, in which case no checksum is computed here at all.
        splits: Vec<u64>,
    },
}

impl Lzma2Plan {
    /// Chooses how to decode one block's LZMA2 coder.
    ///
    /// `threads` is the caller's live ceiling and `adaptive` is whether the
    /// caller asked for a coder that can be widened later even though the
    /// ceiling is one right now. `memory_limit_bytes` is
    /// [`ArchiveLimits::memory_limit_bytes`], and `dict_size` the dictionary
    /// this coder declares.
    ///
    /// # The rule when the budget is too small
    ///
    /// A parallel decode holds whole runs, so it needs room the dictionary
    /// number says nothing about. What is left of the caller's budget after
    /// the decoder's own footprint is the in-flight budget; when the caller
    /// set no budget at all it is `threads` x 256 MiB, which is enough to keep
    /// one 128 MiB run per thread moving (the run size `7zz -mmt=on` picks).
    ///
    /// If that leaves less than 32 MiB, **the coder decodes single-threaded
    /// rather than failing**. A memory limit is a statement about what the
    /// caller can afford, not a request to decode in parallel; refusing the
    /// archive because it cannot *also* be decoded quickly would turn a
    /// performance knob into a correctness one. The single-threaded decoder
    /// streams, so it needs nothing beyond the dictionary — which the reader
    /// has already checked against the same budget, and which is what would
    /// actually refuse the archive.
    ///
    /// [`ArchiveLimits::memory_limit_bytes`]: crate::ArchiveLimits::memory_limit_bytes
    pub(crate) fn for_block(
        threads: u32,
        adaptive: bool,
        memory_limit_bytes: u64,
        dict_size: u32,
        control: &Arc<Lzma2Control>,
        splits: &[u64],
    ) -> Self {
        if threads <= 1 && !adaptive {
            return Self::SingleThreaded;
        }
        let Some(memory_limit) = Self::mt_budget(threads, memory_limit_bytes, dict_size) else {
            return Self::SingleThreaded;
        };
        Self::Adaptive {
            threads,
            memory_limit,
            control: Arc::clone(control),
            splits: splits.to_vec(),
        }
    }

    /// The in-flight budget, or `None` when it is too small to be worth
    /// engaging the parallel path.
    fn mt_budget(threads: u32, memory_limit_bytes: u64, dict_size: u32) -> Option<u64> {
        let budget = if memory_limit_bytes == u64::MAX {
            u64::from(threads).saturating_mul(MT_BUDGET_PER_THREAD_BYTES)
        } else {
            let own = u64::from(dict_size).saturating_add(LZ_STATE_BYTES);
            memory_limit_bytes.saturating_sub(own)
        };
        (budget >= MT_MIN_BUDGET_BYTES).then_some(budget)
    }
}

/// The LZMA2 coder of a block, single-threaded or adaptive.
///
/// Both are a plain `Read` that produces the block's bytes in order, so
/// everything above them — the rest of the coder chain, the CRC verification,
/// the block-completion hook — is the same code either way.
pub(crate) enum Lzma2Coder<R: Read> {
    SingleThreaded(Box<Lzma2Reader<R>>),
    Adaptive(Box<Lzma2MtReader<R>>),
}

impl<R: Read> Read for Lzma2Coder<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::SingleThreaded(r) => r.read(buf),
            Self::Adaptive(r) => r.read(buf),
        }
    }
}

/// A `Read` over [`Lzma2AdaptiveDecoder`], pulling from the coder below.
///
/// The adaptive decoder is a feed/drain machine: this is the thin loop that
/// turns it back into a reader for callers who have a stream in hand rather
/// than one arriving. `crate::Lzma2BlockFeeder` is the same decoder with the
/// loop left to the caller.
pub(crate) struct Lzma2MtReader<R: Read> {
    decoder: Lzma2AdaptiveDecoder,
    input: R,
    control: Arc<Lzma2Control>,
    /// The ceiling currently applied to `decoder`, so that a caller that does
    /// not change it costs one relaxed load per block of output.
    applied_threads: u32,
    inbuf: Vec<u8>,
    in_pos: usize,
    input_done: bool,
    out: Vec<u8>,
    out_pos: usize,
    finished: bool,
    /// Whether the workers are computing checksums to be collected.
    checksums: bool,
    /// Packed bytes handed to the decoder so far, to notice a stream whose
    /// read-ahead is buying nothing. See [`MT_NO_WORKER_GIVE_UP_BYTES`].
    fed_total: u64,
}

impl<R: Read> Lzma2MtReader<R> {
    fn new(
        input: R,
        dict_prop: u8,
        threads: u32,
        memory_limit: u64,
        control: Arc<Lzma2Control>,
        splits: &[u64],
    ) -> Result<Self, std::io::Error> {
        let options = Lzma2MtOptions {
            threads: threads as usize,
            memory_limit,
        };
        let mut decoder = Lzma2AdaptiveDecoder::new(dict_prop, &options).map_err(decode_error)?;
        let checksums = !splits.is_empty();
        if checksums {
            // The worker that produced the bytes checksums them, before it
            // queues to hand the block on. Nothing downstream of here —
            // neither this reader nor the caller consuming it — then has to
            // touch the bytes a second time to know a file's CRC-32.
            decoder.set_checksum(
                &ChecksumPlan::new(Checksum::Crc32).with_split_points(splits.iter().copied()),
            );
        }
        control.engage();
        control.set_threads(threads);
        Ok(Self {
            decoder,
            input,
            control,
            applied_threads: threads,
            inbuf: Vec::new(),
            in_pos: 0,
            input_done: false,
            out: Vec::new(),
            out_pos: 0,
            finished: false,
            checksums,
            fed_total: 0,
        })
    }

    /// Moves the checksums the workers computed into the shared folder, where
    /// the reader's per-file verification picks them up. Cheap: a handful of
    /// `(offset, len, crc)` triples per block, never the bytes.
    fn collect_checks(&mut self) {
        if !self.checksums {
            return;
        }
        for block in self.decoder.take_checks() {
            self.control.fold_segments(&block.segments);
        }
    }

    /// Publishes what the caller decides on: the backlog, the run index and
    /// what is held in memory.
    fn publish(&self) {
        self.control
            .pending_runs
            .store(self.decoder.pending_runs(), Ordering::Relaxed);
        self.control
            .runs_claimed
            .store(self.decoder.runs_claimed(), Ordering::Relaxed);
        self.control
            .in_flight_bytes
            .store(self.decoder.in_flight_bytes(), Ordering::Relaxed);
        self.control.spawned_threads.store(
            u32::try_from(self.decoder.spawned_threads()).unwrap_or(u32::MAX),
            Ordering::Relaxed,
        );
    }

    /// Applies a thread count the caller changed since the last look. The
    /// decoder itself defers it to the next run boundary.
    fn sync_threads(&mut self) {
        let want = self.control.threads();
        if want != self.applied_threads {
            self.decoder.set_threads(want as usize);
            self.applied_threads = want;
        }
    }

    /// Feeds the packed stream until every worker has something to do, the
    /// decoder is holding as much as its budget allows, or the input is
    /// exhausted. Returns whether anything was fed.
    ///
    /// Feeding one chunk per call would be enough to keep a single-threaded
    /// decoder busy, and is exactly what starves a parallel one: a run is
    /// dispatched to a worker only once it has arrived *whole*, so a decoder
    /// holding one chunk has at most one incomplete run and nothing to give
    /// anybody. The runs `7zz -mmt=on` writes are 128 MiB.
    ///
    /// The other end is just as wrong: a `drain` decodes everything the bytes
    /// fed so far allow, so feeding the whole packed stream would decode the
    /// whole block into this reader's buffer before the caller saw its first
    /// byte. So feed until there is a complete run waiting for every thread —
    /// past that point more input buys no more parallelism, only memory — or
    /// until one run per thread has been fed — past that point more input
    /// buys no more parallelism, only memory — or until the decoder says it is
    /// full, which is the budget [`Lzma2Plan::for_block`] chose.
    fn pump_input(&mut self) -> std::io::Result<bool> {
        let mut fed = false;
        let target = feed_target(
            self.applied_threads,
            self.fed_total,
            self.decoder.spawned_threads(),
            self.decoder.memory_limit(),
        );
        loop {
            if self.decoder.in_flight_bytes() >= target {
                break;
            }
            if self.in_pos == self.inbuf.len() {
                if self.input_done {
                    break;
                }
                self.inbuf.resize(MT_INPUT_CHUNK, 0);
                let mut filled = 0;
                while filled < self.inbuf.len() {
                    match self.input.read(&mut self.inbuf[filled..])? {
                        0 => break,
                        n => filled += n,
                    }
                }
                self.inbuf.truncate(filled);
                self.in_pos = 0;
                if filled == 0 {
                    self.input_done = true;
                    self.decoder.end_of_input();
                    break;
                }
            }
            let offered = self.inbuf.len() - self.in_pos;
            let taken = self
                .decoder
                .feed(&self.inbuf[self.in_pos..])
                .map_err(decode_error)?;
            self.in_pos += taken;
            self.fed_total += taken as u64;
            fed |= taken > 0;
            if taken < offered {
                // The decoder is holding all it is allowed to.
                break;
            }
        }
        Ok(fed)
    }
}

/// How far ahead of the decoder to read, in packed bytes.
///
/// One run per thread, capped by the budget — and abandoned entirely once
/// enough has been read with no worker to show for it, which is what a stream
/// with no run boundaries looks like from here.
fn feed_target(threads: u32, fed_total: u64, spawned: usize, memory_limit: u64) -> u64 {
    if fed_total > MT_NO_WORKER_GIVE_UP_BYTES && spawned == 0 {
        return MT_INPUT_CHUNK as u64;
    }
    u64::from(threads.max(1))
        .saturating_mul(MT_FEED_PER_THREAD_BYTES)
        .min(memory_limit)
}

impl<R: Read> Read for Lzma2MtReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.out_pos < self.out.len() {
                let n = (self.out.len() - self.out_pos).min(buf.len());
                buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
                self.out_pos += n;
                return Ok(n);
            }
            if self.finished || buf.is_empty() {
                return Ok(0);
            }

            self.out.clear();
            self.out_pos = 0;
            self.sync_threads();

            // A `drain` decodes everything the bytes fed so far allow, so it
            // can hand over far more than `buf` holds. What fits goes straight
            // into the caller's buffer and only the rest is buffered here:
            // output this reader copies twice is output it pays for twice.
            //
            // `drain` borrows the decoder mutably and the sink needs the spill
            // buffer, so the buffer is lent to the call and taken back.
            let mut direct = 0usize;
            let mut out = std::mem::take(&mut self.out);
            let status = self.decoder.drain(|_offset, bytes| {
                let rest = if direct < buf.len() {
                    let n = (buf.len() - direct).min(bytes.len());
                    buf[direct..direct + n].copy_from_slice(&bytes[..n]);
                    direct += n;
                    &bytes[n..]
                } else {
                    bytes
                };
                if !rest.is_empty() {
                    out.extend_from_slice(rest);
                }
            });
            self.out = out;
            let status = match status {
                Ok(status) => status,
                Err(err) => {
                    self.control.finish();
                    return Err(decode_error(err));
                }
            };
            self.collect_checks();
            self.publish();

            match status {
                DrainStatus::Finished => {
                    self.finished = true;
                    self.control.finish();
                }
                DrainStatus::Progress => {}
                DrainStatus::NeedsMoreInput => {
                    if !self.pump_input()? && self.input_done && direct == 0 && self.out.is_empty()
                    {
                        // End of the packed stream with no end marker: the
                        // stream is short, which is a corrupt archive rather
                        // than a decode that can continue.
                        self.control.finish();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "LZMA2 stream ended without its end marker",
                        ));
                    }
                }
            }

            if direct > 0 {
                return Ok(direct);
            }
        }
    }
}

fn decode_error(err: lzma_fast::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, err)
}

/// Builds the LZMA2 decoder for a 7z coder.
pub(crate) fn lzma2_decoder<R: Read>(
    input: R,
    properties: &[u8],
    plan: Lzma2Plan,
) -> Result<Lzma2Coder<R>, std::io::Error> {
    let dict_prop = properties[0];
    match plan {
        Lzma2Plan::SingleThreaded => Ok(Lzma2Coder::SingleThreaded(Box::new(Lzma2Reader::new(
            input, dict_prop,
        )?))),
        Lzma2Plan::Adaptive {
            threads,
            memory_limit,
            control,
            splits,
        } => Ok(Lzma2Coder::Adaptive(Box::new(Lzma2MtReader::new(
            input,
            dict_prop,
            threads,
            memory_limit,
            control,
            &splits,
        )?))),
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

    /// The documented rule: a budget too small for a parallel decode picks
    /// the single-threaded coder, and never an error.
    #[test]
    fn a_budget_too_small_for_threads_degrades_to_single_threaded() {
        let control = Arc::new(Lzma2Control::new(8));
        let dict = 32 << 20;

        // Room for the dictionary and 64 MiB besides: parallel.
        let plan = Lzma2Plan::for_block(
            8,
            false,
            (32 << 20) + (64 << 20) + LZ_STATE_BYTES,
            dict,
            &control,
            &[],
        );
        assert!(matches!(plan, Lzma2Plan::Adaptive { .. }));

        // Room for the dictionary and almost nothing else: single-threaded,
        // not a memory-limit error.
        let plan = Lzma2Plan::for_block(8, false, (32 << 20) + (1 << 20), dict, &control, &[]);
        assert!(matches!(plan, Lzma2Plan::SingleThreaded));
    }

    /// One thread is the default and costs nothing: no control block, no
    /// adaptive decoder, exactly the coder the fork shipped before.
    #[test]
    fn one_thread_is_the_plain_reader_unless_the_caller_asks_to_widen_later() {
        let control = Arc::new(Lzma2Control::new(1));
        assert!(matches!(
            Lzma2Plan::for_block(1, false, u64::MAX, 1 << 20, &control, &[]),
            Lzma2Plan::SingleThreaded
        ));
        assert!(matches!(
            Lzma2Plan::for_block(1, true, u64::MAX, 1 << 20, &control, &[]),
            Lzma2Plan::Adaptive { threads: 1, .. }
        ));
    }

    /// With no caller budget the in-flight ceiling scales with the threads
    /// asked for, so eight threads can hold eight of `7zz`'s 128 MiB runs.
    #[test]
    fn an_unset_budget_scales_with_the_thread_count() {
        assert_eq!(
            Lzma2Plan::mt_budget(8, u64::MAX, 32 << 20),
            Some(8 * MT_BUDGET_PER_THREAD_BYTES)
        );
    }

    #[test]
    fn lzma_dictionary_comes_from_the_last_four_property_bytes() {
        let mut props = vec![0x5D];
        props.extend_from_slice(&(8u32 << 20).to_le_bytes());
        assert_eq!(lzma_dictionary_size(&props).unwrap(), 8 << 20);
        assert!(lzma_dictionary_size(&[0x5D, 0, 0]).is_err());
    }
}

#[cfg(test)]
mod feed_tests {
    use super::{
        MT_BUDGET_PER_THREAD_BYTES, MT_FEED_PER_THREAD_BYTES, MT_INPUT_CHUNK,
        MT_NO_WORKER_GIVE_UP_BYTES, feed_target,
    };

    #[test]
    fn it_reads_one_run_ahead_per_thread() {
        let budget = 8 * MT_BUDGET_PER_THREAD_BYTES;
        assert_eq!(
            feed_target(8, 0, 1, budget),
            8 * MT_FEED_PER_THREAD_BYTES,
            "a run per thread is what a worker per thread needs"
        );
    }

    #[test]
    fn it_never_reads_further_ahead_than_the_budget() {
        assert_eq!(feed_target(8, 0, 1, 64 << 20), 64 << 20);
    }

    #[test]
    fn it_stops_reading_ahead_when_no_worker_has_appeared() {
        let budget = 8 * MT_BUDGET_PER_THREAD_BYTES;
        // A stream with no dictionary resets: nothing can ever be dispatched,
        // so reading ahead would buffer the whole archive to gain nothing.
        assert_eq!(
            feed_target(8, MT_NO_WORKER_GIVE_UP_BYTES + 1, 0, budget),
            MT_INPUT_CHUNK as u64
        );
        // And it starts again the moment one does appear.
        assert_eq!(
            feed_target(8, MT_NO_WORKER_GIVE_UP_BYTES + 1, 1, budget),
            8 * MT_FEED_PER_THREAD_BYTES
        );
    }
}
