# Contributors

`sevenz-fast` is a fork. Most of the code in this repository was written by
other people, and the copyright and the Apache-2.0 licence of the upstream
project carry over unchanged.

## Upstream: sevenz-rust2

<https://github.com/hasenbanck/sevenz-rust2> — maintained by **Nils Hasenbanck**,
itself a fork of the unmaintained `sevenz-rust` by **dyz1990**.

Upstream authors, by commit count at the fork point (`12ed7c8`, post-v0.22.2):

- Nils Hasenbanck
- dyz1990
- Shun Sakai
- Ben Kollar
- Brian Frazho
- João M. Bezerra
- LoveSy
- Ulysses Horkan
- Dso Tsin
- ttyS3
- makro
- sftse
- rwv
- and everyone else in `git log upstream/main`

The BCJ, BCJ2 and delta filters under `src/codec/filter/` are vendored from
[`lzma-rust2`](https://github.com/hasenbanck/lzma-rust2) 0.20.1, also by Nils
Hasenbanck, also Apache-2.0.

## This fork

- NZB Man (maintainer)

The LZMA/LZMA2 decoding this fork exists to use lives in
[`lzma-fast`](https://github.com/scryer-media/lzma-fast), a port of Igor
Pavlov's public-domain LZMA SDK.
