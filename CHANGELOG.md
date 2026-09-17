# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Fork

`sevenz-fast` is a fork of [sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2)
taken at upstream `12ed7c8` (post-v0.22.2). This section is the exhaustive list
of how it differs from that commit, and it is the checklist a rebase is checked
against. Upstream's own changelog continues below, unchanged.

### Packaging

- Released with `cargo xtask release` and `.github/workflows/release.yml`
  (crates.io trusted publishing on a signed `v<version>` tag); see
  `docs/publishing.md`. The crate archive no longer carries the repository's
  CI, hook and agent files.

- Crate renamed to `sevenz-fast`; the Rust module paths and the public API stay
  upstream's, so a consumer's migration is `sevenz_rust2::` → `sevenz_fast::`.
- Version starts at `0.23.0`, one minor above the upstream base, to make the
  lineage obvious. The `0.23.0` entries below were unreleased upstream at the fork
  point; they are part of the fork base and ship with it.
- MSRV raised from 1.93 to 1.97.1, which `lzma-fast` requires. Pinned in
  `rust-toolchain.toml`.
- `Cargo.lock` is committed (upstream ignores it) so CI can run `--locked` and
  `cargo audit` has something to audit.
- Repository hygiene adopted from the scryer-media house style: SHA-pinned CI
  (`fmt`, `clippy -D warnings`, four-platform tests, MSRV, docs, package),
  `security` (cargo-audit, zizmor), `codeql` + scorecard, gitleaks pre-commit
  hooks, renovate, issue templates, `AGENTS.md`, `SECURITY.md`,
  `CONTRIBUTORS.md`. Upstream's `.github/workflows/rust.yml` and
  `.github/dependabot.yml` were removed as duplicates of these.
- The `lzma-fast` dependency is a path dependency for now. It becomes a
  crates.io version pin before this crate is published.

### Decoding

- LZMA (`0x030101`) and LZMA2 (`0x21`) are decoded by
  [`lzma-fast`](https://github.com/scryer-media/lzma-fast) instead of
  `lzma-rust2`. On the fixtures in `docs/benchmarking.md` this is 1.62x
  upstream's throughput and level with `7zz t -mmt=1`, where upstream was 1.3x
  behind it.
- The parallel LZMA2 reader decodes no more than the caller's buffer holds
  per read. It used to drain everything the decoder had ready — at eight
  threads, up to a whole run per worker — spill the excess and copy it a
  second time on the way out. Measured on x86 at eight threads, a gigabyte
  went from 4.82 s to 4.59 s. Needs lzma-fast 0.3.0 for `drain_upto`.
- `lzma-rust2` has left the library's runtime dependency graph. It remains an
  optional dependency behind the `compress` feature, which still uses its LZMA
  and LZMA2 *encoders*; with `--no-default-features` the graph is
  `sevenz-fast → lzma-fast → crc-fast` and nothing else.
- The BCJ, BCJ2 and delta filters are vendored into `src/codec/filter/` from
  `lzma-rust2` 0.20.1 (Apache-2.0, same licence), which is what lets
  `lzma-rust2` leave the decode graph rather than be carried for three filters.
  `src/codec/filter/mod.rs` documents the provenance and the mechanical
  changes.
- LZMA2 decodes on several threads through `lzma-fast`'s `Lzma2AdaptiveDecoder`
  — a stream is cut at the dictionary resets that make a *run* independently
  decodable, and runs are decoded on workers while output stays in order.
  Upstream's `Decoder::Lzma2Mt` (`lzma-rust2`'s `Lzma2ReaderMt`) is gone;
  `Lzma2Plan` in `src/codec/lzma_fast.rs` is the one place that chooses, and
  the rest of the decode chain, the CRC verification and the completion hook
  are unchanged by the choice.
- **The thread count now defaults to one**, where upstream defaults to
  `available_parallelism()`. A library does not decide on its own to occupy
  every core, or to hold the memory that doing so costs; the consumer asks with
  `set_threads`/`with_threads`. Existing callers that set a thread count
  explicitly are unaffected; callers that set none get today's memory and
  today's behaviour.
- **No CRC-32 is computed on the thread delivering the bytes when LZMA2 is
  decoding in parallel.** The block's file boundaries go to the coder as
  checksum split points, each worker checksums the pieces of the block it
  produced before it queues to hand that block on, and this crate folds those
  pieces into each file's CRC-32 — and into the block's — with
  `crc32_combine`, which costs the same whatever the pieces weigh. The
  verifying reader is then not built at all. What is verified, and when a
  corrupt archive is refused, does not change; a test asserts the refusal
  happens under the parallel path, where the streaming check is gone.
  Single-threaded blocks, and blocks whose LZMA2 output passes through a
  filter (BCJ, delta, BCJ2) on its way out, keep the streaming checksum: for
  the first the consuming thread is the decoding thread, and for the second
  the bytes the workers saw are not the bytes the file is made of.
- The reader walks the LZMA2 chunk headers itself and feeds the decoder **whole
  runs only**, a gigabyte or a run per thread ahead, whichever is more. Three
  things had to be true at once. A run reaches a worker only once it has
  arrived whole, so feeding a chunk at a time leaves nothing to dispatch and
  decodes everything inline. Feeding without limit decodes the whole block into
  memory before the caller sees a byte, because one `drain` decodes everything
  the fed bytes allow. And a run whose end the decoder has not seen is taken by
  its chase decoder, which finishes it on the calling thread with dispatch
  switched off — so a batch must be big enough that the one run this costs at
  its end is overlapped by several rounds of worker work, not comparable to it.
  Feeding "a run per thread, then the rest" cost 1.50x against the bare
  parallel decoder at two threads; feeding whole runs a gigabyte at a time is
  within 2% of it at every thread count measured. `docs/benchmarking.md` has
  the numbers and the memory that buys them, and
  `docs/lzma-fast-requests.md` the upstream change that would make the
  gigabyte unnecessary.
- And when a good look at a stream has found no run boundary at all — what
  `7zz -mmt=1` writes is one run from beginning to end — the reader stops
  getting ahead altogether and lets the decoder stream it, rather than
  buffering an entire archive to hand to a single worker at the end. That is
  decided from the boundaries the reader has scanned, not from whether a worker
  has appeared, so it holds however large the read-ahead is.
- A thread count above one is a request, not a promise. A stream with no
  dictionary resets (what `7zz -mmt=1` writes) decodes single-threaded because
  there is nothing to cut, and a block whose memory budget has no room to hold
  runs in flight **degrades to single-threaded rather than failing** — a limit
  states what the caller can afford, not that the archive must be refused.
- LZMA1 is now subject to the same dictionary memory limit as LZMA2. Upstream
  bounded only LZMA2, so an archive declaring a 4 GiB LZMA1 dictionary would
  try to allocate it.
- CRC-32 comes from `crc-fast` (via `lzma-fast`) rather than `crc32fast`; the
  `crc32fast` dependency is gone.
- The LZMA coder's properties are length-checked before being sliced, instead
  of panicking on a short field.

### Safety against hostile archives

Upstream trusts the numbers in a 7z header. This fork bounds every one of them
before the allocation or the work it sizes, against both the bytes the archive
actually has and the caller's `ArchiveLimits`. The model, the table of limits
and the audit are in `docs/security.md`; in summary:

- Header counts — files, blocks, coders, pack streams, sub-streams, bind pairs
  — no longer reserve on a claim. Neither does a name, a names blob, a coder
  properties field or a compressed header's declared unpacked size.
- A compressed header that decodes to another one is refused rather than
  followed, and a coder graph is validated before anything is built from it:
  indices in range, no stream bound or packed twice, exactly one unbound
  output, and no cycle.
- A coder's stream counts are bounded (`max_streams_per_coder`), which is what
  makes the graph's linear searches constant work instead of quadratic.
- Pack streams must end inside the file, so a header cannot aim a decode at an
  arbitrary offset.
- LZMA and LZMA2 dictionaries are clamped to the coder's declared unpacked
  size, the way 7-Zip reduces them: a 4 GiB dictionary on a 1 KiB stream is
  memory that would be allocated and never read. An archive that upstream
  would refuse for its budget can now decode.
- zstd's declared window is bounded by the caller's budget, and otherwise by
  the 128 MiB the reference decoder itself will not exceed.
- The AES key-derivation work factor is the caller's `max_aes_cycles_power`,
  never above 24 — the header field is six bits, so 2^63 rounds is otherwise
  expressible from a file a caller merely opened.
- The declared output and output-to-packed ratio can be bounded
  (`max_unpack_bytes`, `max_unpack_ratio`), refusing a bomb before a byte is
  decoded.
- Entry names are reported as unsafe (`ArchiveEntry::is_unsafe_path`) or
  refused outright (`ArchiveLimits::reject_unsafe_paths`), and symlink entries
  are surfaced rather than silently written as small text files.
- `fuzz/` has libFuzzer targets on the header parser and the coder graph,
  running under a counting allocator that fails a run that allocates more than
  the limits allow.

None of this changes what a well-formed archive does: the defaults are one to
two orders of magnitude above the largest legitimate values, and the
differential matrix against `7zz` stays green.

### Container API (additions only)

Everything here is new surface; no upstream signature changed meaning.

- `ArchiveLimits`, with `ArchiveReader::with_limits`,
  `Archive::read_with_limits` and `BlockDecoder::with_limits`: one model for
  what an archive is allowed to claim, checked *before* the allocation or the
  work it bounds. Two affordability limits — `memory_limit_bytes` (bounding
  `Archive::decoder_memory_estimate` and then each coder as it is built) and
  `max_end_header_bytes` (before the header is buffered) — and the structural
  bounds `max_header_unpacked_bytes`, `max_header_depth`, `max_entries`,
  `max_name_bytes`, `max_total_name_bytes`, `max_coders_per_block`,
  `max_streams_per_coder`, `max_total_coders`, `max_unpack_bytes`,
  `max_unpack_ratio`, `max_aes_cycles_power` and `reject_unsafe_paths`. Every
  field documents the attack it closes and defaults to a value no legitimate
  archive reaches; `ArchiveLimits::unlimited()` removes the structural ones for
  a caller reading archives it wrote itself. `docs/security.md` is the table.
- `Error::LimitExceeded { what: Limit, limit, requested }`, so a consumer can
  report which bound stopped a read rather than "corrupt archive", with
  `Limit::field()` naming the `ArchiveLimits` field and `Error::limit_hit()`
  mapping every way a limit is reported — including `EndHeaderTooLarge` and
  `MemoryLimited`, which predate it — onto the one enum.
- `ArchiveEntry::is_unsafe_path()` / `unsafe_path_reason()`, `is_symlink()`
  and `unix_mode()`: whether a stored name would escape an extraction
  directory and which way, and whether the entry is a link whose content is a
  target rather than a file. `Error::UnsafeEntryName` is what
  `reject_unsafe_paths` raises.
- `Archive::decoder_memory_estimate() -> Result<u64, UnsizedCoder>` and
  `coder_memory_estimate(&Coder)`: what a single-threaded decode of the archive
  needs, as the largest block's coder chain. The per-coder table is in the
  rustdoc.
- Read-only views of what the header already parsed:
  `Archive::num_unpack_sub_streams()`, `Archive::sub_stream(index)`,
  `Archive::block_sub_streams(block)` (per-entry size and CRC-32),
  `Archive::block_pack_streams(block)` (absolute `(offset, size)` ranges, four
  of them for a BCJ2 block) and `Archive::block_coders(block)`.
- `ArchiveReader::block_decoder(block_index)` borrows the reader instead of
  consuming it, so a consumer parses the header once and decodes blocks from
  the same source; `source_mut()` and `into_source()` complete that. This is
  what replaces opening the archive twice because the constructor took
  ownership.
- `ArchiveReader::set_block_complete_hook` / `clear_block_complete_hook`,
  called with a `BlockCompletion { block_index, unpacked_size, crc_verified }`
  once a block has been decoded in full and its checksum verified. A block the
  caller stopped short of is not reported.
- `ArchiveReader::set_threads` / `with_threads` / `threads` (upstream's
  `set_thread_count` still works and forwards), and
  `set_adaptive_lzma2` / `with_adaptive_lzma2`, which builds a block's LZMA2
  coder so it can widen later even while the count is one. `BlockDecoder` has
  the same four.
- `ArchiveReader::lzma2_handle() -> Lzma2Handle`: an owned, `Send + Sync`
  handle taken before a decode starts and used while it runs — a decode borrows
  the reader for its duration, so the live knob cannot be a method on it.
  `Lzma2Handle::set_threads(n)` takes effect at the next LZMA2 run boundary,
  which is a dictionary reset and therefore lossless; `1` decodes the next run
  inline on the calling thread, spawning nothing. `Lzma2Handle::progress()` and
  `ArchiveReader::lzma2_progress()` report an `Lzma2Progress { block_index,
  threads, spawned_threads, pending_runs, runs_claimed, in_flight_bytes }` —
  `pending_runs` is the backlog of complete runs an adaptive caller widens on,
  and `runs_claimed` is the run index of the block being decoded.
- `ArchiveReader::set_sub_stream_complete_hook` / `clear_…`, called with a
  `SubStreamCompletion { block_index, sub_stream_index, file_index,
  unpacked_offset, len, crc32 }` as each file's checksum becomes final. The
  decoder computes that checksum anyway, to check it against the header; this
  hands the value over instead of discarding it, so a consumer reporting
  per-file integrity never reads the bytes a second time.
- `ArchiveReader::set_verify_checksums` / `with_verify_checksums` (and
  `BlockDecoder::with_verify_checksums`) turn the header's CRC-32 checks off
  for a consumer that verifies the bytes by other means — a PAR2 set over the
  extracted files, say — and does not want to pay for the same assurance
  twice. On by default. With it off a corrupt archive decodes into corrupt
  bytes without complaint, which is why it is spelled out rather than implied
  by a thread count or a limit.
- `crc32_combine(a, b, len_b)` and `CrcFolder`, re-exported from `lzma-fast`:
  the checksum of two pieces joined, and a heap of `(offset, len, crc32)`
  pieces folded into any range they cover, for a consumer folding across
  boundaries this crate does not know about, such as across blocks. Re-exported
  rather than reimplemented, so a consumer folds with the same implementation
  the workers checksummed with.
- `Error::BlockDecode { block_index, packed_offset, kind, message }` with
  `BlockErrorKind::{Corrupted, ChecksumMismatch, UnsupportedMethod, Io,
  Password}`: corruption now says which block and which byte range, distinctly
  from an I/O failure on the source or a method this build cannot decode. An
  error raised by the *caller's* own callback is passed through untouched — the
  decode chain is wrapped so the two can be told apart.

### Documentation

- `docs/benchmarking.md` — the harness, the acceptance gate and the numbers.
- `docs/lzma-fast-requests.md` — the API this crate would like from
  `lzma-fast`, with the exact signatures and the local work-around for each.
  The parallel-decoder request landed and is recorded as such; the AES and
  key-derivation requests were withdrawn when that crate removed both on
  purpose; what is outstanding is worker-side checksums on the adaptive
  decoder and a run index over a stream not yet being decoded.
- `AGENTS.md` gained the rule this fork is now held to: no CRC-32 is computed
  in a serialised section of the multi-threaded path, and thread counts
  default to one.

### Cryptography

- **The AES decoder decrypts in the caller's buffer.** It used to read the
  packed stream 512 bytes at a time into a fixed array, decrypt into a `Vec`
  and copy that into the caller's buffer — two million reads and two full
  copies of the payload for a 1 GiB store-mode archive. It now fills the
  caller's buffer with ciphertext and decrypts it in place, so reads are the
  caller's size and the payload is copied zero extra times. The only state kept
  is the ≤15 ciphertext bytes that did not complete a block, plus one block of
  plaintext for callers that read less than 16 bytes at a time; neither grows
  with the stream, so the memory estimate is unchanged. On the Linux bench box
  a 1 GiB store-mode AES archive went from 0.862 s to 0.282 s, which is under
  half of `7zz t -p` on the same fixture and the sum of what the cipher, the
  read and the CRC cost on their own.

- Cryptography for the `aes256` coder — SHA-256 *and* AES-256-CBC — goes
  through one internal backend module, `src/crypto_backend.rs`. SHA-256 comes
  from `lzma-fast`'s crypto module. The default backend is
  `aws-lc-rs` (feature `aws-lc-crypto`, in `default`), the scryer-media house
  convention shared with `lzma-fast` and `rarpar`; `native-crypto` selects
  RustCrypto's `sha2` and **takes precedence** when both are compiled, so a
  consumer who cannot build C uses `default-features = false` plus
  `native-crypto`.
- **AES-256-CBC is this crate's own code, but not its own backend.** `lzma-fast`
  removed AES and the 7z key derivation deliberately — 7z cryptography is this
  crate's job — so the cipher lives in `src/crypto_backend.rs`, and it follows
  the same feature switch SHA-256 does: `aws_lc_rs::cipher::DecryptingKey::cbc`
  (AWS-LC's unpadded CBC mode, no PKCS7 — `StreamingDecryptingKey` is the padded
  one and is not used) on the default lane, RustCrypto's `aes`/`cbc` on
  `native-crypto`, which compiles to AES-NI on x86-64 and to the ARMv8
  cryptography extensions on aarch64. Neither lane needs a streaming API: a
  chunk is decrypted with the current IV and that chunk's last ciphertext
  block, copied out before the in-place decrypt, is the next chunk's IV. The
  encoder (`compress`) keeps RustCrypto's `cbc::Encryptor`.
  `aws-lc-rs` is a direct optional dependency on the pin and features
  `lzma-fast` uses, so a build with both crates resolves one copy of AWS-LC.
- `aes256` no longer implies a backend: enabling it with neither
  `aws-lc-crypto` nor `native-crypto` is a compile error. A consumer migrating
  from upstream with `default-features = false, features = ["aes256", …]` adds
  `"aws-lc-crypto"` to that list.
- New `sevenz_fast::crypto_backend() -> &'static str`, reporting which backend
  a build selected, for consumers who want to assert on it.
- The `aes` and `cbc` dependencies are enabled by `native-crypto` (decryption)
  and by `compress` (encryption); the AWS-LC lane does not compile them. The
  direct `sha2` dependency is gone.
- When both backends are compiled, a differential test checks they agree on
  SHA-256, on the 7z key derivation and on AES-256-CBC — the NIST SP 800-38A
  F.2.6 vector on each lane, whole-buffer equality at several sizes, chunked
  chaining at 1/2/3/5/13 blocks per call against the one-shot result, empty
  calls and the partial-block refusal. The selected lane is also checked
  against NIST on its own, block by block as well as in one call, and SHA-256
  against its own vectors. `7zAes.c`'s two special cycle counts (`0x3F`, `>= 0x40`) have tests of
  their own.

### Testing

- `tests/differential_7zz_tests.rs`: archives built with `7zz a` across the
  method matrix are extracted with both `7zz x` and this crate and compared
  byte for byte. Skips itself when `7zz` is not on `PATH`.
- `tests/lzma2_mt_tests.rs`: the multi-threaded path against `7zz x -so` at 1,
  2, 8 and every thread; that the parallel path is actually engaged and reports
  its backlog; that moving the thread count mid-archive changes no byte; that a
  memory limit too small for threads degrades to single-threaded instead of
  failing; that the block-completion hook still fires once per block; that
  per-file checksums match the header and a checksum taken over the bytes by an
  unrelated implementation; and that folding checksums equals checksumming the
  whole. The differential matrix now runs every case through three lanes —
  one thread, eight threads, and the adaptive coder at one thread.
- `tools/decode-bench`: times `7zz t` (all threads and `-mmt=1`), upstream
  `sevenz-rust2` 0.22.2 at 16 threads (its `Lzma2ReaderMt` path) and this fork
  at 1, 2, 8 and every thread, on the same archive in one session. Results in
  `docs/benchmarking.md`.
- The vendored BCJ round-trip tests generate their sample data instead of
  reading the binary fixtures `lzma-rust2` keeps in its repository, which are
  not ours to vendor.

## 0.23.0 - 2026-09-17

The first release of `sevenz-fast`. It is everything in the [Fork](#fork)
section above, together with these upstream changes, which were unreleased at
the fork point and ship here for the first time.

### Added

- `LzmaOptions::set_nice_len`, `LzmaOptions::set_dictionary_size` and `Lzma2Options::set_nice_len`.
- `EncoderConfiguration` can be built from `LzmaOptions` with `into()`.

### Fixed

- Reject unsupported LZMA and LZMA2 encoder dictionary sizes with an error before allocating the encoder or starting
  compression workers, avoiding capacity-overflow panics.
- Improved decompression performance for non-solid 7z archives containing many files.
- Threaded LZMA2 decoding of archives made of many small runs - what `7zz -mx1` writes for data that does not
  compress, 1 MiB a run - scales with the thread count. It took two changes: `lzma-fast` 0.3.0 no longer moves its
  whole input buffer after every run, and the reader here stops reading ahead once there are two runs per thread
  waiting and tops that up before every drain, instead of reading a gigabyte before the first worker started. A 1 GiB
  archive of that shape at 18 threads: 4.6 s before, 1.1 s after, level with `7zz t`.

## 0.22.2 - 2026-08-25

### Fixed

- Fixed build process and tests for all cargo feature combinations (#126)

## 0.22.1 - 2026-08-25

### Fixed

- Fixed memory check on 32-bit systems when using PPMd (#134)

## 0.22.0 - 2026-08-23

### Changed

- Bumped `lzma_rust2` to 0.20.
- Expose coder's properties (#131, thanks @lukr54)
- Let a solid pack be compressed away from the writer (#132, thanks @lukr54)

## 0.21.5 - 2026-08-16

### Changed

- Bumped `lzma_rust2` to 0.19.
- Batch AES-CBC decryption to improve performance.

## 0.21.4 - 2026-08-01

### Fixed

- Fixed an integer overflow when summing attacker-controlled coder stream counts while parsing a block header. Malformed
  archives panicked in debug builds and bypassed the stream-count bound in release builds. They are now rejected with an
  error. (#127, thanks @tyrex-vberthier)

## 0.21.3 - 2026-07-05

### Changed

- Cache most recent AES key derivation to improve performance (#118, thanks @jdlien)

### Fixed

- Hardened the library against malicious or malformed archives that could otherwise cause a panic, an infinite loop, or
  an unbounded allocation while parsing or decoding.

## 0.21.2 - 2026-07-01

### Fixed

- Decode 7z folders that layer a single-input filter (e.g. Delta) on top of a BCJ2 coder (`Method = Delta BCJ2`). These
  folders previously failed to decode with an
  `Unsupported method` error because the decoder required the folder's final output coder to be BCJ2 itself. (#117,
  thanks @trevorWieland)

## 0.21.1 - 2026-06-23

### Fixed

- Fix security issue were malicious 7z files could write files outside the destination directory. Reported by @lintowe
  (#116)

## 0.21.0 - 2026-04-25

### Changed

- Bumped MSRV to 1.93 (required by `nt-time` 0.15)

### Updated

- Bumped `nt-time` to 0.15 (#105)
- Bumped `aes` to 0.9 and `cbc` to 0.2 (cipher 0.5) (#111, #110)

### Fixed

- `K_ANTI` property block was not written for archives containing only anti-items, so anti-items were extracted as
  0-byte files instead of acting as deletion markers (#112, thanks @uraf)
- `compress_path` no longer emits a spurious entry for the root directory itself when compressing a directory tree (#79,
  thanks @super1207)

## 0.20.2 - 2026-02-24

### Fixed

- Updated internal dependencies `lzma-rust2` to v0.16 and `getrandom` to v0.4

## 0.20.1 - 2026-01-01

### Fixed

- Fix bug where small headers of encrypted files were not encrypted (#98)

## 0.20.0 - 2025-12-07

### Changed

- Expose public getters for streaming archive data @vihu (#93)

### Fixed

- Brotli codec decompression is not working without compress (#95)

## 0.19.4 - 2025-11-26

### Fixed

- Fixed "AES256 properties too short" bug (#91)

## 0.19.3 - 2025-11-01

### Updated

- Target ppmd-rust v1. Since ppmd-rust is stable and has a major version, we will target it directly to reduce
  maintenance burden.

## 0.19.2 - 2025-10-29

### Updated

- Target lzma-rust2 v0.15.

## 0.19.1 - 2025-09-23

### Fixed

- Removed too strict debug_assert as reported in #81
- Removed incompatibility issues when using the `ArchiveWriter::push_source_path()` with single files. We now properly
  handle both cases were a file or a directory is given to the function.

## 0.19.0 - 2025-09-20

### Changed

- Breaking change: Rename identifiers to follow Rust API Guidelines @sorairolake (#59)

### Updated

- Target lzma-rust2 v0.14.
- Updated the documentation to include the BCJ2 codec again. It was never gone, only overlooked.

### Fixed

- Improved compatibility with third party utilities when creating 7z files (esp. Windows's Explorer and macOS's Finder).
- Improve docs.rs feature compatibility @sorairolake (#56)

### Removed

- Removed the dependency to `byteorder`.

## 0.18.0 - 2025-08-25

### Added

- Add support for BCJ IA64 filter.
- Add support for BCJ RISC-V filter.
- Added encoder support for all BCJ filter.

### Updated

- Target lzma-rust2 v0.10.

### Changed

- Changed one of LZMA2Options::from_level_mt's parameter name.

## 0.17.1 - 2025-07-26

### Updated

- Target lzma-rust2 v0.6.

## 0.17.0 - 2025-07-25

### Added

- Added muti-threading support for LZMA2 compression & decompression.
- Added `ArchiveReader::set_thread_count()` to set the thread count when decoding with multiple threads. Only LZMA2 is
  supported right now. `ArchiveReader` will set the thread_count with the help of
  `std::thread::available_parallelism` as default.
- Added missing documentation for the public API.

### Changed

- Split `LZMA2Option` into `LZMAOptions` and `LZMA2Options` to better support multi-threading encoding for LZMA2.
- Removed `EncoderOptions::Num` enum, which means encoders needs to be configured with their respected option struct.

## 0.16.2 - 2025-07-16

### Updated

- Target lzma-rust2 v0.4 which increases the encoding speed.

## 0.16.1 - 2025-07-12

### Updated

- Target lzma-rust2 v0.3 which increases the decoding speed dramatically (around 50% on x86_64).

## 0.16.0 - 2025-07-04

### Changed

- Removed a lot of exports that were not used in the public facing API.
- Expose all compression and filter method options via the `encoder_options` module.
- Renamed the following structs in an attempt to make the API easier to navigate:
    - `SevenZArchiveEntry` -> `ArchiveEntry`
    - `SevenZReader` -> `ArchiveReader`
    - `SevenZWriter` -> `ArchiveWriter`
    - `SevenZMethod` -> `EncoderMethod`
    - `SevenZMethodConfiguration` -> `EncoderConfiguration`
    - `MethodOptions` -> `EncoderOptions`
    - `Folder` -> `Block`
- Renamed all instances of "folder" in the API to "directory" instead to clearly distinct from the "folder" in the 7z
  format specification and align with the std fs module.
- Internal `Archive`, `SteamMap`, `Coder` and `Block` fields are removed from the public API.
- What the 7z specification calls "folders" are a false friend and we instead call them "blocks".
- Every API that takes a password now uses the `Password` struct instead. Added helper functions to create password from
  strings and raw bytes.
- The needed features for WASM changed. Please use the "default_wasm" feature.
- Removed the hard dependency to `nt-time` by creating our own time struct `NtTime`. The feature can be used to convert
  to and from `nt-time::FileTime`.

### Removed

- Removed the dependency to `bit-set` and `filetime_creation`.
- `nt-time` dependency is now optional.

### Updated

- PPMd crate to version v1.2, which fixes the compatibility issues with existing 7z files using PPMd7.

## 0.15.3 - 2025-06-28

### Fixed

- Properly finish PPMd files.

## 0.15.2 - 2025-06-27

### Fixed

- No functional updates.
- Moved lzma-rust2 and ppmd-rust into their own crates.

## 0.15.1 - 2025-06-22

### Fixed

- Updated outdated documentation.

## 0.15.0 - 2025-06-22

### Updated

- Target optional dependency bzip2 v0.6.

### Changed

- The PPMd crate is using a native Rust version that is validated with Miri. All 7zip supported compression algorithms
  (LZMA, LZMA2, BZIP2 and PPMd) have now Rust native implementations and don't need a C compiler.
- All standard compression algorithms of 7zip are enabled by default.
- Use default feature of lz4_flex

## 0.14.1 - 2025-06-02

### Added

- Added support for ARM64 BCJ filter (by Benkol003).
- Add support for encoding LZ4 with skippable frames.

### Fixed

- Fixed decompressing LZ4 that contain skippable frames.

### Changed

- Use lz4_flex instead of lz4, which is a faster Rust native implementation. The only downside is, that only one
  compression level is supported.

## 0.13.2 - 2025-05-01

### Fixed

- Loose version restrictions on some dependencies.

## 0.13.1 - 2025-04-05

### Fixed

- Fix broken WASM build.

## 0.13.0 - 2025-03-31

### Fixed

- Fix the bug where the optional compression methods did not finish properly and created invalid entries when writing 7z
  archives.

### Changed

- Moved `CountingWriter` from lzma to sevenz crate, since it was an internal implementation detail of the sevenz crate.
- Remove implicit way to call `finish()` by `calling write(&[])` on the lzma and lzma2 writer. This was again an
  implementation detail of the sevenz. `finish()` now also takes `self`, like other compression libraries.

### Added

- `LZMAWriter` and `LZMA2Writer` now expose the inner writer and also return it when calling `finish(self)`.

## 0.12.1 - 2025-03-10

### Fixed

- Fix broken LZ4 feature compilation

## 0.12.0 - 2025-02-28

### Added

- Support for Delta filter compression
- Support for PPDm compression / decompression

## 0.11.0 - 2025-02-26

### Updated

- Updated dependency nt-time to 0.11

### Changed

- Introduced a new feature "util", so that users can deactivate those functionality, if not needed
- Added a lot of documentation tags for docs.rs
- Update of nt-time introduce the need to increase MSRV to 1.85

## 0.10.0 - 2025-02-26

### Changed

- The Brotli codec now supports the skippable frame encoding found in zstdmt (used by 7zip ZS and NanaZip). This is the
  default format, since it seems to be the default for user facing programs. The default data a frame contains is 128
  KiB.

## 0.9.0 - 2025-02-25

### Added

- Add `SevenZReader::file_compression_methods()`.

### Fixed

- Improve compatibility with third party programs (tested with 7-Zip ZS 1.5.6)
  bzip2, LZ4 and ZSTD now work flawless. BROTLI isn't compatible right now.

## 0.8.0 - 2025-02-25

### Added

- Optional support for compressing / decompressing with BROTLI
- Optional support for compressing with bzip2
- Optional support for compressing / decompressing with DEFLATE
- Optional support for compressing / decompressing with LZ4
- Optional support for compressing with ZStandard

### Changed

- Replaced all unsafe code from sevenz-rust2 and lzma-rust2 with safe alternatives
- sha2 is now an optional dependency that is activated when using the aes256 features
- Update of the documentation
- Removed the need to provide the length of a reader when reading an archive file

## 0.7.0 - 2025-02-24

This release should be mostly compatible with the old 0.6.1. The breaking changes are:

- Previously deprecated functionality were removed
- Spelling issues were fixed in some API names

### Added

- `SevenZReader::readFile()` added
- `SevenZArchiveEntry::new_file()` and `SevenZArchiveEntry::new_folder()` factory functions added
- Compression method COPY is now supported

### Changed

- Updated dependency bit-set v0.8
- Updated dependency bzip2 v0.5
- Updated dependency nt-time v0.10
- Use crc32fast instead of crc

### Fixed

- Replaces insecure usage of rand with getrandom
- Renamed `get_memery_usage()` into `get_memory_usage()`
- Renamed `compress_encypted()` into `compress_encrypted()`

### Removed

- Removed deprecated `FolderDecoder`. Use `BlockDecoder` instead
- Removed deprecated `SevenZWriter::create_archive_entry()`. Use `SevenZArchiveEntry::from_path()` instead

## 0.6.1 - 2024-07-17

- Fixed 'unsafe precondition (s) violated'. Closed #63

## 0.6.0 - 2024-04-05

- Added support for encrypted headers - close #55
- Return a consistent error in case the password is invalid - close #53

## 0.5.4 - 2023-12-13

- Added docs
- Renamed `FolderDecoder` to `BlockDecoder`
- Added method to compress paths in non-solid mode
- Fixed entry's compressed_size is always 0 when reading archives.

## 0.5.3

Fixed 'Too many open files' Reduce unnecessary public items #37

## 0.5.2 - 2023-08-24

Fixed file separator issue on Windows system #35

## 0.5.1 - 2023-08-23

Sub crate `lzma-rust` code optimization

## 0.5.0 - 2023-08-19

- Added support for BCJ2.
- Added multi-thread decompress example

## 0.4.3 - 2023-06-16

- Support write encoded header
- Added `LZMAWriter`

## 0.4.2 - 2023-06-10

- Removed unsafe code
- Changed `SevenZWriter.finish` method return inner writer
- Added wasm compress function
- Updates bzip dependency to the patch version of 0.4.4 ([#23](https://github.com/dyz1990/sevenz-rust/pull/23))

## 0.4.1 - 2023-06-07

- Fixed unable to build without default features

## 0.4.0 - 2023-06-03 - Solid compression

## 0.3.0 - 2023-06-02 - Encrypted compression

- Added Encrypted compression

## 0.2.11 - 2023-05-24

- Fixed numerical overflow

## 0.2.10 - 2023-04-18

- Change to use nt-time crate ([#20](https://github.com/dyz1990/sevenz-rust/pull/20))
- Fix typo ([#18](https://github.com/dyz1990/sevenz-rust/pull/18))
- make function generics less restrictive ([#17](https://github.com/dyz1990/sevenz-rust/pull/17))
- Solve warnings ([#16](https://github.com/dyz1990/sevenz-rust/pull/16))
- run rustfmt on code ([#15](https://github.com/dyz1990/sevenz-rust/pull/15))

## 0.2.9 - 2023-03-16

- Added bzip2 support ([#14](https://github.com/dyz1990/sevenz-rust/pull/14))

## 0.2.8 - 2023-03-06

- Fixed write bitset bugs

## 0.2.7 - 2023-03-05

- Fixed bug while read files info

## 0.2.6 - 2023-02-23

- Added zstd support and use enhanced filetime lib ([#11](https://github.com/dyz1990/sevenz-rust/pull/11))
- Fixed lzma encoder bugs

## 0.2.4 - 2023-02-16

- Changed return entry ref when pushing to writer ([#10](https://github.com/dyz1990/sevenz-rust/pull/10))

## 0.2.3 - 2023-02-07

- Fixed incorrect handling of 7z time

## 0.2.2 - 2023-01-31 - Create sub crate `lzma-rust`

- Move mod `lzma` to sub crate `lzma-rust`
- Modify GitHub Actions to run tests with --all-features

## 0.2.0 - 2023-01-08 - Added compression supporting

- Added compression supporting

## 0.1.5 - 2022-11-01 - Encrypted 7z files decompression supported

- Added aes256sha256 decode method
- Added wasm support
- Added new tests (for Delta and Copy) and GitHub Actions CI ([#5](https://github.com/dyz1990/sevenz-rust/pull/5))
  by [bfrazho](https://github.com/bfrazho)

## 0.1.4 - 2022-09-20 - Replace lzma/lzma2 decoder

- Chnaged new lzma/lzma2 decoder

## 0.1.3 - 2022-09-18 - add more bcj filters

- Added bcj arm/ppc/sparc and delta filters
- Added test for bcj x86 ([#3](https://github.com/dyz1990/sevenz-rust/pull/3)) by [bfrazho](https://github.com/bfrazho)

## 0.1.2 - 2022-09-14 - bcj x86 filter supported

- Added bcj x86 filter
- Added LZMA tests ([#2](https://github.com/dyz1990/sevenz-rust/pull/2)) by [bfrazho](https://github.com/bfrazho)
- Fixed extract empty file

## 0.1.1 - 2022-08-10 - Modify decompression function

## 0.1.0 - 2022-08-10 - Decompression
