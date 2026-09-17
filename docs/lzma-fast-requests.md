# API requests to `lzma-fast`

This fork consumes [`lzma-fast`](https://github.com/scryer-media/lzma-fast) and
does not modify it. Where a different signature there would let this crate do
something it currently cannot, the request is written here, with the exact
shape that is wanted and what it is for, so the two crates can move
independently. Nothing in this file is a commitment by that crate.

Anything listed here is worked around locally in the meantime, and the
work-around is named per item.

## What has landed

### The parallel LZMA2 decoder — landed, and this fork is on it

The request that mattered. `lzma-fast` now has two multi-threaded drivers, and
this crate uses the second one:

```rust
pub struct Lzma2MtOptions { pub threads: usize, pub memory_limit: u64 }

// The faithful port of 7-Zip's Lzma2DecMt.c over MtDec.c: pull from a Read,
// own the threads for the whole call.
impl Lzma2ParallelDecoder {
    pub fn new(dict_prop: u8, options: &Lzma2MtOptions) -> Result<Self, Error>;
    pub fn decode<R: Read + Send, W: Write + Send>(self, input: R, out: W) -> io::Result<u64>;
}
pub struct Lzma2ParallelReader<R: Read + Send + 'static>; // its Read adapter

// Fed input, polled output, mode switched mid-stream.
impl Lzma2AdaptiveDecoder {
    pub fn new(dict_prop: u8, options: &Lzma2MtOptions) -> Result<Self, Error>;
    pub fn set_threads(&mut self, threads: usize);
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, Error>;
    pub fn end_of_input(&mut self);
    pub fn drain<F: FnMut(u64, &[u8])>(&mut self, sink: F) -> Result<DrainStatus, Error>;
    pub fn pending_runs(&self) -> usize;
    pub fn backlog(&self) -> impl ExactSizeIterator<Item = &Lzma2Run>;
    pub fn runs_claimed(&self) -> u64;
    pub fn in_flight_bytes(&self) -> u64;
    pub fn spawned_threads(&self) -> usize;
    pub fn cancel(&mut self);
}
```

Every constraint this file used to list is met by it:

| asked for | how it is met |
| --- | --- |
| in-order output | `drain` delivers `(offset, bytes)` in order by default |
| memory limit enforced before allocation | `Lzma2MtOptions::memory_limit`; dispatch and `feed` are both refused rather than exceeding it |
| cancellation | `cancel()` stops the workers and joins them before returning |
| lossless mode switch at run boundaries | `set_threads(n)`, applied at the next boundary; `1` decodes inline |
| a public run index | `Lzma2RunScanner`, and `pending_runs`/`backlog`/`runs_claimed` on the decoder |
| lazy workers | none are spawned until a run is actually dispatched |
| partial-run single-threaded decode | the chase decoder decodes a run whose tail has not arrived |

**Why this crate uses the adaptive driver and not the ring.** The ring's `Read`
adapter spawns it behind a thread and so requires `Read + Send + 'static`; a 7z
coder's input is a bounded view of the caller's archive source, which is
neither. The adaptive decoder borrows nothing — the work it hands to threads is
owned copies of complete runs — and it is the only one of the two that can
change its thread count mid-stream, which is what a consumer chasing a download
needs. Both find runs with the same scanner and decode them with the same
decoder, so the bytes are identical. See `src/codec/lzma_fast.rs`.

### AES-256-CBC and the 7z key derivation — withdrawn

This file used to ask for an AES *encryptor* and for the 7z key derivation on a
caller-chosen backend. `lzma-fast` has since removed AES and that derivation
altogether, deliberately: 7z cryptography is this crate's job, and that crate is
LZMA, LZMA2 and the xz container. Both requests are therefore withdrawn, not
outstanding.

What this crate does instead, in `src/crypto_backend.rs`: AES-256-CBC is
written here and follows the same backend feature SHA-256 does —
`aws_lc_rs::cipher::DecryptingKey::cbc` (AWS-LC's unpadded CBC mode) by
default, RustCrypto's `aes`/`cbc` under `native-crypto` — while SHA-256 comes
from `lzma_fast::crypto::awslc` or `lzma_fast::crypto::rustcrypto`. Neither
lane needs a streaming CBC API: each chunk is decrypted with the current IV and
its last ciphertext block becomes the next chunk's. The 7z derivation runs over
a local `Sha256Like` trait, and the cipher over an `Aes256CbcLike` one, which
is what makes the cross-backend differential tests possible.

### Worker-side checksums — landed, and this fork folds them

**The rule this serves:** no CRC-32 is computed in a serialised section of the
multi-threaded path. Checksumming is O(bytes), and the in-order section is the
one place where the core count does not help, so a checksum taken there is a
tax that grows with the archive.

`crates/lzma-fast/src/crc.rs` and `crates/lzma-fast/src/mt/checksum.rs`, with
the plan wired into the **adaptive** decoder — which is the one a 7z coder can
use:

```rust
pub fn crc32_combine(crc_a: u32, crc_b: u32, len_b: u64) -> u32;
pub fn crc64_xz_combine(crc_a: u64, crc_b: u64, len_b: u64) -> u64;
pub struct CrcFolder<W: Foldable>;            // push (offset, len, crc), ask for a range

pub enum Checksum { None, Crc32, Crc64Xz, Sha256 }
pub struct Segment { pub offset: u64, pub len: u64, pub check: SegmentCheck }
pub struct BlockChecks { pub unpacked_offset: u64, pub len: u64, pub digest: Option<[u8; 32]>, pub segments: Vec<Segment> }
pub struct ChecksumPlan { /* Checksum + split points */ }
impl ChecksumPlan {
    pub fn new(kind: Checksum) -> Self;
    pub fn with_split_points<I: IntoIterator<Item = u64>>(self, points: I) -> Self;
}

impl Lzma2AdaptiveDecoder {
    pub fn set_checksum(&mut self, plan: &ChecksumPlan);
    pub fn take_checks(&mut self) -> Vec<BlockChecks>;
}
```

What this crate does with it, in `BlockDecoder::for_each_entries`: the block's
file boundaries — the cumulative sub-stream sizes — go in as split points, the
segments that come back out of `take_checks` are pushed into a `CrcFolder`
shared through `Lzma2Control`, and each file's CRC-32 is
`CrcFolder::range(offset, len)`. The verifying reader is then not built at all,
neither per file nor per block, so the thread delivering the bytes does no
checksumming. `crc32_combine` and `CrcFolder` are re-exported from this crate
rather than reimplemented.

Two blocks keep the streaming checksum, and both are inherent rather than
pending:

- a block whose LZMA2 output passes through a filter (BCJ, delta, BCJ2) — the
  workers' bytes are not the file's bytes, and the filter itself runs on the
  consuming thread;
- a block decoded single-threaded, where the consuming thread *is* the
  decoding thread and there is no serialised section to keep clear.

## Outstanding

### A chase decoder that stands aside while a worker is free

`Lzma2AdaptiveDecoder::drain` gives the run at its cursor to the chase decoder
— the single-threaded one, on the caller's thread — whenever `dispatch` finds
no *complete* run there, and a run is complete only once the chunk header that
follows it has arrived. It then keeps that run to the end: `st_in_run` turns
dispatch off until the chase is through.

That is right for the case it was built for, a stream still arriving. It is
wrong for a stream already on disk, where it happens once per batch of fed
bytes and is not a small cost: the chase decodes in 1 MiB steps and no worker
may start a run while it runs, so at two threads a batch of two runs spent as
long chasing the third as the two workers spent on the other two. Measured on
`mt.7z`, 8 runs of 128 MiB: 15.9 s against 10.6 s for the same decoder's own
parallel path, 1.50x, with half the output produced in 1 MiB steps.

```rust
impl Lzma2AdaptiveDecoder {
    /// Whether the run at the cursor may be decoded on the calling thread when
    /// no worker can take it yet. On by default, which is the arriving-stream
    /// case; off for a caller that would rather wait than serialise.
    pub fn set_chase(&mut self, chase: bool);
}
```

Or, without a knob at all: return `Dispatch::Busy` rather than `None` when
`outstanding != 0` and there is no complete run at the cursor. A worker is
already running; there is something to wait for; waiting is what `drain`
already does everywhere else.

**Work-around until then.** This crate walks the chunk headers itself
(`Lzma2RunScanner`, which is public — thank you) and feeds only whole runs, so
the chase is never handed a run whose bytes are still coming in this reader's
buffer; and the batch is sized so that the one run the chase does take at the
end of it is overlapped by several rounds of worker work rather than being a
third of the batch. That costs a gigabyte of read-ahead, and about two of peak
memory, to hide something that would otherwise cost nothing at all.

### A drain that stops when the caller's buffer is full

`Lzma2AdaptiveDecoder::drain` decodes everything the bytes fed so far allow and
hands each block to a sink as `(offset, &[u8])`. A 7z coder is a `Read`: it is
asked for as much as fits in a caller's buffer, which is typically 64 KiB to a
few MiB, while a block out of the parallel path is a whole run — 128 MiB for an
archive written by `7zz -mmt=on`. So every byte is copied once into a spill
buffer here and once out of it again, and the spill buffer grows to the size of
everything fed.

```rust
impl Lzma2AdaptiveDecoder {
    /// As `drain`, but stops once `sink` has been handed `limit` bytes,
    /// keeping the rest for the next call.
    pub fn drain_upto<F>(&mut self, limit: usize, sink: F) -> Result<DrainStatus, Error>;
}
```

That would let this crate hand the decoder the caller's own buffer and copy
nothing. It would also make the *memory* of a parallel decode a function of
what is in flight rather than of what has been fed.

**Work-around until then.** What fits in the caller's buffer is copied into it
directly from the sink and only the overflow is buffered, and the feed is
capped so that a single `drain` cannot decode an entire archive into memory —
though the cap has to stay large for the reason in the previous request, so the
spill is measured in hundreds of megabytes rather than in the tens it should
be.

### A run index over a stream this crate has not started decoding

`Lzma2RunScanner` is public and answers this, but it has to be fed the bytes.
A consumer deciding *whether* to widen before it commits to a decode would
rather ask about a packed range it can seek to:

```rust
pub fn run_boundaries<R: Read + Seek>(source: R, dict_prop: u8) -> io::Result<Vec<Lzma2Run>>;
```

**Work-around until then.** The decoder reports its own backlog as it goes
(`Lzma2Progress::pending_runs`), which is enough for an adaptive caller: it
widens on a backlog it can already see rather than on a prediction.
