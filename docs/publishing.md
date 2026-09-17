# Publishing

Releases are cut with `scripts/release.sh` and published by
`.github/workflows/release.yml`. The script runs every check the workflow
runs, so a tag that reaches GitHub is one the workflow will accept.

`sevenz-fast` is published after the `lzma-fast` version it pins, never
before: the manifest reaches `lzma-fast` through crates.io, and the release
script refuses to tag while a `path` is still on that dependency.

## One-time setup, before the first release

1. **Publish the first version by hand.** crates.io only lets a workflow
   publish a crate that already exists, so the first version goes up from a
   machine with a crates.io token:

   ```sh
   cargo publish --locked
   ```

   Do this after the tag is pushed and the `verify` job is green; the
   `publish` job of that first run will fail, which is expected.
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
3. `scripts/release.sh --dry-run`, then `scripts/release.sh`. The script
   refuses a dirty tree, an unsigned HEAD, a branch other than `main`, a
   changelog section still marked unreleased, a remaining `path` on
   `lzma-fast`, and a tag that already exists. It then runs the tests and a
   `cargo publish --dry-run`, creates the signed tag `v<version>` and pushes
   it.
4. The workflow verifies the tag against the manifest, tests, publishes to
   crates.io and creates the GitHub release with the changelog section as its
   notes.
