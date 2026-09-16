# sevenz-fast

[![ci](https://github.com/scryer-media/sevenz-fast/actions/workflows/ci.yml/badge.svg)](https://github.com/scryer-media/sevenz-fast/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/sevenz-fast.svg)](https://crates.io/crates/sevenz-fast)
[![docs.rs](https://docs.rs/sevenz-fast/badge.svg)](https://docs.rs/sevenz-fast)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/scryer-media/sevenz-fast/badge)](https://securityscorecards.dev/viewer/?uri=github.com/scryer-media/sevenz-fast)

A 7z compressor/decompressor in pure Rust.

## This is a fork

`sevenz-fast` is a fork of
**[sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2)** by Nils
Hasenbanck (Apache-2.0), taken at upstream `12ed7c8`, just after v0.22.2.
Nearly all of the code here is upstream's, and it stays that way: the public
API and the Rust module paths are upstream's, so migrating is
`sevenz_rust2::` → `sevenz_fast::` plus whatever new calls you want.

Two things differ, and they are the whole reason the fork exists.

### 1. LZMA and LZMA2 decode with `lzma-fast`

Upstream decodes LZMA/LZMA2 with
[`lzma-rust2`](https://github.com/hasenbanck/lzma-rust2), which is the fastest
pure-Rust LZMA decoder published but still about 1.3x slower than 7-Zip
single-threaded. This fork routes those two coders to
[`lzma-fast`](https://github.com/scryer-media/lzma-fast), a port of Igor
Pavlov's reference decoder (including the LZMA SDK's assembly loops) that is at
parity with `7zz` single-threaded. `lzma-rust2` is no longer in the library's
runtime dependency graph; it remains only behind the `compress` feature, whose
encoders are what write archives.

Measured numbers live in [docs/benchmarking.md](docs/benchmarking.md).

### 2. Container API a streaming consumer needs

Added, never altered — every upstream signature still means what it did. See
[CHANGELOG.md](CHANGELOG.md), section `## Fork`, for the exhaustive list of
divergences, and [AGENTS.md](AGENTS.md) for how the fork is rebased onto
upstream.

### Crypto backends

The 7z `aes256` coder needs AES-256-CBC and SHA-256. The default backend is
`aws-lc-rs`; enabling `native-crypto` switches to RustCrypto (`aes`, `cbc`,
`sha2`) and takes precedence, so a consumer that cannot build C can use
`default-features = false` with `aes256, native-crypto`. CRC-32 is `crc-fast`.

## Usage

```toml
[dependencies]
sevenz-fast = "0.23"
```

Decompress "data/sample.7z" to "data/sample":

```rust
sevenz_fast::decompress_file("data/sample.7z", "data/sample").expect("complete");
```

### Decompress an encrypted 7z file

```rust
sevenz_fast::decompress_file_with_password("path/to/encrypted.7z", "data/sample", "password".into()).expect("complete");
```

## Compression

```rust
sevenz_fast::compress_to_path("examples/data/sample", "examples/data/sample.7z").expect("compress ok");
```

### Compress with AES encryption

```rust
sevenz_fast::compress_to_path_encrypted("examples/data/sample", "examples/data/sample.7z", "password".into()).expect("compress ok");
```

### Advanced usage

#### Solid compression

Solid archives can compress better, but decompressing one file needs all the
data in front of it decompressed too.

```rust
use sevenz_fast::*;

let mut writer = ArchiveWriter::create("dest.7z").expect("create writer ok");
writer.push_source_path("path/to/compress", |_| true).expect("pack ok");
writer.finish().expect("compress ok");
```

#### Configure the compression methods

```rust
use sevenz_fast::*;

let mut writer = ArchiveWriter::create("dest.7z").expect("create writer ok");
writer.set_content_methods(vec![
    encoder_options::AesEncoderOptions::new("password".into()).into(),
    encoder_options::Lzma2Options::from_level(9).into(),
]);
writer.push_source_path("path/to/compress", |_| true).expect("pack ok");
writer.finish().expect("compress ok");
```

### Supported codecs and filters

| Codec       | Decompression | Compression |
|-------------|---------------|-------------|
| COPY        | ✓            | ✓          |
| LZMA        | ✓ (lzma-fast) | ✓          |
| LZMA2       | ✓ (lzma-fast) | ✓          |
| BROTLI (*)  | ✓            | ✓          |
| BZIP2       | ✓            | ✓          |
| DEFLATE (*) | ✓            | ✓          |
| PPMD        | ✓            | ✓          |
| LZ4 (*)     | ✓            | ✓          |
| ZSTD (*)    | ✓            | ✓          |

(*) Require optional cargo feature.

| Filter        | Decompression | Compression |
|---------------|---------------|-------------|
| BCJ X86       | ✓            | ✓          |
| BCJ ARM       | ✓            | ✓          |
| BCJ ARM64     | ✓            | ✓          |
| BCJ ARM_THUMB | ✓            | ✓          |
| BCJ RISC_V    | ✓            | ✓          |
| BCJ PPC       | ✓            | ✓          |
| BCJ SPARC     | ✓            | ✓          |
| BCJ IA64      | ✓            | ✓          |
| BCJ2          | ✓            |             |
| DELTA         | ✓            | ✓          |

### WASM support

WASM is supported, but not with the default features: `aws-lc-rs` does not
build for `wasm32`, so the WASM feature set uses the RustCrypto backend.

```bash
RUSTFLAGS='--cfg getrandom_backend="wasm_js"' cargo build --target wasm32-unknown-unknown --no-default-features --features=default_wasm
```

## Licence

This crate is licensed under the
[Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0),
the same as upstream, and upstream's copyright notices are unchanged.

Note that `lzma-fast`, which this crate depends on for LZMA/LZMA2 decoding, is
licensed GPL-3.0-or-later. This crate's own source stays Apache-2.0, but a
binary that links it together with `lzma-fast` is a combined work under the
GPL. If that is a problem for you, upstream `sevenz-rust2` is the crate you
want.
