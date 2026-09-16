# API requests to `lzma-fast`

This fork consumes [`lzma-fast`](https://github.com/scryer-media/lzma-fast) and
does not modify it. Where a different signature there would let this crate do
something it currently cannot, the request is written here, with the exact
shape that is wanted and what it is for, so the two crates can move
independently. Nothing in this file is a commitment by that crate.

Anything listed here is worked around locally in the meantime, and the
work-around is named per item.

## 1. A parallel LZMA2 reader (the one that matters)

`src/codec/lzma_fast.rs` is the only file in this crate that names `lzma-fast`,
and it is deliberately shaped around a `Lzma2Plan` enum with one variant today:

```rust
pub(crate) enum Lzma2Plan {
    SingleThreaded,
}
```

When the multi-threaded decoder lands, this grows one variant and the decoder
in `src/decoder.rs` does not change at all. The plan carries what the parallel
decoder needs and this crate is the only thing that knows: the packed stream's
byte range in the archive, a thread count, a memory ceiling, and a way to stop.

### What this crate can supply

A 7z reader knows, before it decodes a block, exactly where that block's packed
bytes are: `Archive::block_pack_streams(block_index)` returns absolute
`(offset, size)` ranges, and the source is `Read + Seek`. So a seekable view of
one pack stream can be handed over, not just a `Read`:

```rust
impl Lzma2Reader {
    /// A reader that decodes `source` — one whole LZMA2 stream, seekable —
    /// across `threads` workers, in order, within `memory_limit` bytes.
    pub fn parallel<S: Read + Seek + Send>(
        source: S,
        dict_prop: u8,
        threads: NonZeroU32,
        memory_limit: u64,
    ) -> io::Result<Self>;
}
```

### The constraints it has to satisfy

These are this crate's requirements on whatever shape the API takes, in the
order they matter:

1. **In-order output.** The reader stays a `std::io::Read` whose bytes are the
   stream's bytes. A consumer that reassembles out-of-order chunks itself is a
   consumer this crate cannot be.
2. **An explicit memory limit, enforced before allocation.** A parallel LZMA2
   decode buffers a whole run of dependent chunks, so its footprint scales with
   the run rather than with the dictionary, and
   `Archive::decoder_memory_estimate` (which models the single-threaded
   decoders) stops describing it. The limit has to be a parameter, and
   exceeding it has to be an error rather than an allocation — this crate
   refuses archives against a caller's budget before it decodes, and it cannot
   do that for a decoder whose appetite it cannot ask about. A
   `Lzma2Reader::parallel_memory_estimate(dict_prop, threads) -> u64` would let
   the estimate stay honest.
3. **Cancellation.** A consumer that gives up — a cancelled download, a closed
   output — must be able to stop the workers without waiting for the stream to
   finish. Dropping the reader is enough if drop joins promptly.
4. **A lossless switch between modes at run boundaries.** Whether a stream can
   be decoded in parallel is a property of how it was *written* (7-Zip's
   `-mmt=on` starts a fresh dictionary per chunk run; `-mmt=1` does not), and
   this crate cannot tell from the coder properties alone. The reader should
   start single-threaded and widen when it reaches a run boundary that allows
   it, rather than failing or silently producing wrong bytes: a stream that
   turns out not to be parallelisable must still decode, at single-threaded
   speed.
5. **A public run index.** `st.7z` and `mt.7z` in the benchmark fixtures differ
   only in how the encoder chunked them, and this crate cannot see the
   difference. Something like
   `Lzma2Reader::run_boundaries(&mut source) -> io::Result<Vec<u64>>` (packed
   offsets of the chunks that reset the dictionary) would let this crate decide
   whether parallel decoding is worth starting, and would let it report a
   damaged run's offset instead of "somewhere in this block".
6. **Lazy, cheap workers.** Most 7z blocks in practice are small. Spawning a
   thread pool per block would cost more than it saves, so workers should be
   created when a second run is actually available, not when the reader is
   constructed.
7. **Partial-run single-threaded decode.** When a run is only partly available
   — the exact case of a download being chased — decoding what has arrived, at
   single-threaded speed, is much better than parking. A parallel reader that
   demands the whole run up front cannot be used by a streaming consumer at
   all.

### Positional output, if a push API is offered instead

If the parallel decoder prefers to hand out decoded pieces rather than be a
`Read`, the pieces must be positional, so that this crate can order them:

```rust
/// `(uncompressed_offset, bytes)`, any order, every byte exactly once.
pub fn decode_parallel(&mut self, sink: impl FnMut(u64, &[u8]) -> io::Result<()>) -> io::Result<()>;
```

### Work-around until then

Every LZMA2 stream is decoded single-threaded, which is already level with
`7zz t -mmt=1` (`docs/benchmarking.md`). Nothing is wrong; the ceiling is one
core.

## 2. An AES-256-CBC *encryptor*

`lzma_fast::crypto` exposes `Aes256Cbc` with `decrypt` only, which is all a 7z
*reader* needs. This crate also writes archives (`compress`), so the encoder in
`src/encryption/aes.rs` still uses the RustCrypto `cbc` encryptor directly, and
`aes`/`cbc` are dependencies of the `compress` feature for that reason alone.

Wanted:

```rust
impl Aes256Cbc {
    /// Encrypts `data` in place and advances the chaining state.
    pub fn encrypt(&mut self, data: &mut [u8]) -> Result<(), CryptoError>;
}
```

on both backends. Then `src/crypto_backend.rs` covers writing as well as
reading, `compress` stops pulling RustCrypto in, and a build with
`aws-lc-crypto` has exactly one AES implementation in it instead of two.

### Work-around until then

The encoder is on RustCrypto regardless of which backend the decoder uses. The
two implementations agree (the differential test in `src/crypto_backend.rs`
checks the parts that overlap), so this is a packaging wart, not a correctness
one.

## 3. `sevenz_key` on a caller-chosen backend

`lzma_fast::crypto::sevenz_key` uses whichever backend *that crate's* features
select, and its precedence is AWS-LC-wins. This crate's convention is the
opposite (`native-crypto` wins), so it cannot call `sevenz_key` without
silently disagreeing with its own documented precedence.

Wanted: the derivation generic over the digest, or exposed per backend —

```rust
pub mod awslc { pub fn sevenz_key(password_utf16le: &[u8], salt: &[u8], cycles: u8) -> Option<[u8; 32]>; }
pub mod rustcrypto { /* the same */ }
```

### Work-around until then

`src/crypto_backend.rs` defines a small `Sha256Like` trait, implements it for
both of `lzma-fast`'s SHA-256 types, and `encryption::aes::derive_key_with`
runs 7-Zip's derivation over it. Roughly fifteen lines, and it is what makes
the cross-backend differential test possible, so this request is low priority.
