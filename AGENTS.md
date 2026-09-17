# Agent and contributor rules for sevenz-fast

These rules apply to every automated agent and every human contributor.

## What this repository is

`sevenz-fast` is a fork of [sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2)
by hasenbanck, Apache-2.0, forked at upstream `12ed7c8` (post-v0.22.2). It
exists for two reasons and no others:

1. **Codec swap.** LZMA (`03 01 01`) and LZMA2 (`21`) decode through
   [`lzma-fast`](https://github.com/scryer-media/lzma-fast), a port of Igor
   Pavlov's reference decoder, instead of `lzma-rust2`. `lzma-rust2` is not in
   the library's runtime dependency graph; it remains only behind the
   `compress` feature, whose encoders upstream needs for writing archives.
2. **Container API.** The 7z detail a streaming consumer needs and upstream
   does not expose: memory limits enforced before allocation, per-member CRCs,
   folder-to-pack-stream byte ranges, a reader that parses once and decodes
   from a caller-supplied `Read + Seek`, typed corruption errors carrying a
   block index and packed offset, and a per-block completion hook.

Everything else should stay byte-for-byte upstream so that rebases are
mechanical. **Rust module paths and the public API are upstream's**, so a
consumer's migration is `sevenz_rust2::` → `sevenz_fast::` plus the new calls.

## Fork rules

1. **Confine the diff.** New behaviour goes in new files under `src/codec/`,
   `src/limits.rs`, `src/crypto_backend.rs` and friends. Touch `src/decoder.rs`
   and `src/reader.rs` only where the swap and the new API genuinely need it.
   Never reformat, rename or "tidy" upstream code: every such hunk is a rebase
   conflict forever.
2. **Never change an upstream public signature.** Add; do not alter. If an
   addition would be a breaking change upstream-side, add a parallel entry
   point (`with_limits` beside `new`) instead.
3. **Record every divergence** in the `## Fork` section of `CHANGELOG.md`, in
   the same commit that creates it. That section is the rebase checklist.
4. **Upstream fixes go upstream.** A bug that is not ours is reported and, if
   possible, fixed at hasenbanck/sevenz-rust2; we take it on the next rebase.

## How to rebase onto upstream

```sh
git fetch upstream
git log --oneline 12ed7c8..upstream/main          # what moved
git diff 12ed7c8..HEAD -- src/ > /tmp/fork.diff   # what we carry
git switch -c chore/rebase-<upstream-tag> main
git rebase --onto upstream/main 12ed7c8
```

Conflicts should appear only in `src/decoder.rs` and `src/reader.rs`. Work
through the `## Fork` changelog section afterwards and confirm every listed
divergence still exists; a silently dropped one is the failure mode. Then move
the fork base commit named at the top of this file, and re-run the extraction
differential matrix in `docs/benchmarking.md`.

## Codec rules

- LZMA and LZMA2 decoding is `lzma-fast`'s, reached only through
  `src/codec/lzma_fast.rs`. Nothing else in the crate names `lzma_fast::`.
  That file is also where the multi-threaded LZMA2 coder lives; see
  `docs/lzma-fast-requests.md`.
- The BCJ, BCJ2 and delta filters are vendored from `lzma-rust2` (Apache-2.0,
  see `src/codec/filter/mod.rs`) so the crate does not carry `lzma-rust2` at
  runtime. Fixes to them belong upstream in `lzma-rust2` as well as here.
- Crypto goes through `src/crypto_backend.rs`: `aws-lc-rs` by default,
  RustCrypto when the `native-crypto` feature is on. Never call a backend
  crate directly from anywhere else.
- CRC-32 is `crc-fast` (via `lzma-fast`'s `crc` module), never `crc32fast`.
- **No CRC-32 is computed in a serialised section of the multi-threaded path.**
  Checksumming is O(bytes) and the in-order section is the one place where the
  core count does not help, so a checksum taken there is a tax that grows with
  the archive. Checksums are computed where the bytes are produced — on the
  worker that decoded them — and folded with `crc32_combine`, which costs the
  same regardless of how long the pieces are. The single-threaded path is
  exempt: it *is* the calling thread, so there is no section to serialise
  against, and so is a block whose LZMA2 output passes through a filter (BCJ,
  delta, BCJ2) on the way out, because the bytes the workers checksummed are
  not the bytes the file is made of — that filter runs on the consuming thread
  and the checksum has to run there with it. Everywhere else, the block's file
  boundaries go to the coder as split points and `Crc32VerifyingReader` is not
  built at all: see `BlockDecoder::file_boundaries` and `Lzma2Control::folded`.
  Verification is not weakened by this — a corrupt block is still refused, and
  a test asserts it under the parallel path.
- Thread counts default to one, everywhere. A library does not decide on its
  own to occupy every core, or to hold the memory that doing so costs; the
  consumer asks.
- **The parallel LZMA2 reader feeds whole runs, and never stops the ring.** The
  decoder gives the run at its cursor to its chase path — single-threaded, on
  the calling thread, with dispatch switched off until that run is done —
  whenever the run's end has not arrived. So this crate walks the chunk headers
  itself and feeds only runs it has seen the end of, and it keeps the batch
  large enough that the one run the chase still takes at the end of a batch is
  overlapped rather than waited on. Both rules are load-bearing: dropping
  either cost 1.5x against a bare parallel decode at two threads. Anything
  changing `pump_input` or the feed constants must be measured at 2, 4 and 8
  threads, not only at the machine's full width, where the whole archive fits
  one batch and the bug is invisible. `SEVENZ_FAST_MT_TRACE=1` prints the phase
  split — how much was chased, how long was spent feeding, how long draining —
  and is how that is checked.

## Reading hostile archives

- **No allocation and no unit of work is sized by a header field without a
  limit check first.** Every number in a 7z header is attacker-chosen. Before
  anything is reserved, walked, decoded or derived from one, it is bounded by
  the bytes the archive actually has *and* by the relevant [`ArchiveLimits`]
  field — a count is one byte and the entry it reserves is a hundred, so the
  byte bound alone is not enough. `Vec::with_capacity(claimed)` and
  `vec![x; claimed]` on an unchecked number are the shape of the bug; the
  parser's `HeaderBounds` is where the check goes.
- A new limit is a documented field of `ArchiveLimits` with a default a
  legitimate 1 GiB archive never reaches, an entry in the table in
  `docs/security.md`, a `Limit` variant so a consumer can say which bound
  stopped it, and a crafted archive in `tests/security_tests.rs`.
- The defaults must never change what a well-formed archive does. The
  differential matrix is what says so.

## Repository hygiene

- Commits are SSH-signed. Never pass `--no-gpg-sign`.
- Never run destructive git working-tree operations (`checkout --`,
  `restore`, `reset --hard`, `clean`, `stash`) in a shared checkout.
- Branch names use gitflow prefixes: `feature/…`, `bugfix/…`, `hotfix/…`.
- Do not push. The maintainer pushes.
- The pre-commit hook (`.githooks/pre-commit`) rejects home paths, the local
  username and secrets. Keep it enabled: `git config core.hooksPath .githooks`.
- Never commit generated archives. The small archives under `tests/resources/`
  and `examples/data/` are the tracked differential corpus; anything larger
  than 1 MiB, or anywhere else, is rejected by the `hygiene` CI lane.
- `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`
  and `cargo test --all-features` must pass before a commit is proposed.
- Any code change bumps the crate version, `Cargo.lock` and `CHANGELOG.md` in
  the same change.
