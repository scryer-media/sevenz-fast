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

`src/crc.rs` and `src/mt/checksum.rs`, with
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

### A drain that stops when the caller's buffer is full — landed, and this fork is on it

`Lzma2AdaptiveDecoder::drain_upto(limit, sink)` (lzma-fast 0.3.0) hands the
sink no more than `limit` bytes and keeps the rest of the block for the next
call. The parallel reader here asks for exactly what the caller's buffer
holds, so a block out of the parallel path — a whole run, 128 MiB for an
archive written by `7zz -mmt=on` — is no longer decoded into a spill buffer
and copied out of it again. On x86 at eight threads that took a gigabyte from
4.82 s to 4.59 s; the spill path stays in the reader as a safety net that is
never taken.

### A chase decoder that stands aside while a worker is free — landed, not yet taken up

`Lzma2AdaptiveDecoder::set_chase(false)` (lzma-fast 0.3.0) makes the
decoder wait for a worker instead of decoding the run at its cursor on the
calling thread when that run's chunk header has not arrived. That was the
request: for a stream already on disk, chasing happened once per batch of fed
bytes and cost, at two threads, as long as the two workers spent on the other
two runs. This fork still uses its own work-around — it walks the chunk
headers itself (`Lzma2RunScanner`) and feeds only whole runs, sized so the
one run the chase does take is overlapped by several rounds of worker work —
which costs a gigabyte of read-ahead to hide the chase. Switching the reader
to `set_chase(false)` and a smaller batch is the open item.

## Outstanding

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
