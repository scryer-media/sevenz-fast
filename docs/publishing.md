# Publishing

Releases are cut with `cargo xtask release` and published by
`.github/workflows/release.yml`. The task (`xtask/src/main.rs`, plain Rust
with no dependencies, so it runs wherever `cargo` does) runs every check the workflow
runs, so a tag that reaches GitHub is one the workflow will accept.

`sevenz-fast` is published after the `lzma-fast` version it pins, never
before: the manifest reaches `lzma-fast` through crates.io, and the release
task refuses to tag while a `path` is still on that dependency.

## One-time setup, before the first release

1. **Publish the first version from a machine with a crates.io token.**
   crates.io only lets a workflow publish a crate that already exists, so the
   first release is cut with

   ```sh
   cargo xtask release --publish
   ```

   which runs the checks, creates the signed tag, runs `cargo publish` and
   then pushes the tag. The workflow sees the version on crates.io, skips its
   own upload and creates the GitHub release.
2. **Turn on trusted publishing** at
   `https://crates.io/crates/sevenz-fast/settings/new-trusted-publisher`:
   repository owner `scryer-media`, repository `sevenz-fast`, workflow
   `release.yml`, environment `crates-io`.
3. **Create the `crates-io` environment** in the GitHub repository settings.
   Restricting it to tag refs `v*` is enough; no secrets are needed, the
   workflow gets a short-lived token from crates.io through OIDC.

The release workflow needs `contents: write` to create the GitHub release,
which it requests for that job only.

## Each release

1. Make sure the `lzma-fast` version named in `Cargo.toml` is on crates.io,
   and that the dependency line carries no `path`. While the two crates are
   developed side by side the line may carry `version` and `path` together;
   `cargo` uses the sibling checkout and `cargo publish` would use the
   version, but CI has no sibling checkout, so the path is dropped before
   the release commit.
2. On `main`, with CI green on the merge commit:
   - set `version` in `Cargo.toml`;
   - make sure `CHANGELOG.md` has a `## <version>` heading that is not marked
     unreleased (the `## Fork` section stays where it is; it is the rebase
     checklist, not a release note);
   - commit, signed.
3. `cargo xtask release --dry-run`, then `cargo xtask release`. The dry run
   works on any branch and on a dirty tree, and lists everything a real run
   would refuse rather than stopping at the first. A real run refuses a dirty
   tree, an unsigned HEAD, a branch other than `main`, a changelog section
   missing or still marked unreleased, a README that shows neither the version
   nor its major.minor, a remaining `path` on `lzma-fast`, and a tag that
   already exists. It then runs the tests in release mode (`--skip-tests`
   leaves them to CI and to the workflow's `verify` job) and a
   `cargo publish --dry-run`, creates the signed tag `v<version>` and pushes
   it.
4. The workflow verifies the tag against the manifest, tests, publishes to
   crates.io and creates the GitHub release with the changelog section as its
   notes.
