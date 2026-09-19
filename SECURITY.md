# Security policy

## Supported versions

Only the latest published release of `sevenz-turbo` receives fixes.

## Reporting a vulnerability

Please do not open a public issue for security problems. Report privately
through GitHub at <https://github.com/scryer-media/sevenz-turbo/security/advisories/new>
(the Security tab, "Report a vulnerability"); private reporting is enabled on
this repository. You will get an acknowledgement within a few days, and a
fix ships as a new release of the crate, credited to you unless you ask
otherwise.

## The threat model, and what bounds it

[`docs/security.md`](docs/security.md) is this crate's model for reading an
archive from a stranger: what is assumed, every limit with its default and the
attack it closes, what a caller sees when one is hit, and how it is checked.
Read it before reporting, and before pointing this crate at untrusted input.

An archive reader's attack surface is its input, and every field in a 7z header
is attacker-controlled. Reports of panics, out-of-bounds reads, unbounded
allocation, path traversal on extraction, or hangs on crafted archives are
security reports and are handled as such. Include the archive if you can.

This crate is a hard fork of
[sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2) and is not kept in
step with it. A bug that exists there too is still fixed and released here;
we do not hold a fix for it, and we do not disclose on its behalf.
