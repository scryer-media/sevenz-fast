# Security policy

## Supported versions

Only the latest published release of `sevenz-fast` receives fixes.

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's private
vulnerability reporting on this repository (Security tab, "Report a
vulnerability"). You will get an acknowledgement within a few days.

An archive reader's attack surface is its input, and every field in a 7z header
is attacker-controlled. Reports of panics, out-of-bounds reads, unbounded
allocation, path traversal on extraction, or hangs on crafted archives are
security reports and are handled as such. Include the archive if you can.

If the problem is inherited from upstream
[sevenz-rust2](https://github.com/hasenbanck/sevenz-rust2) rather than created
by this fork, tell us, and we will coordinate with upstream rather than
disclose ahead of them.
