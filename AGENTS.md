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
  That file is also where the multi-threaded LZMA2 API lands when it ships; see
  `docs/lzma-fast-requests.md`.
- The BCJ, BCJ2 and delta filters are vendored from `lzma-rust2` (Apache-2.0,
  see `src/codec/filter/mod.rs`) so the crate does not carry `lzma-rust2` at
  runtime. Fixes to them belong upstream in `lzma-rust2` as well as here.
- Crypto goes through `src/crypto_backend.rs`: `aws-lc-rs` by default,
  RustCrypto when the `native-crypto` feature is on. Never call a backend
  crate directly from anywhere else.
- CRC-32 is `crc-fast` (via `lzma-fast`'s `crc` module), never `crc32fast`.

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
