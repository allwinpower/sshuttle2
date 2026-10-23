# Releasing

A version tag publishes a release. [release.yml](../.github/workflows/release.yml) runs [build-release.yml](../.github/workflows/build-release.yml), which builds every file and installs it on several Linux distributions. Then it publishes the GitHub Release.

## Steps

1. Open a pull request that:
   - sets `version` under `[workspace.package]` in `Cargo.toml`, then runs `cargo update -w` to update `Cargo.lock`;
   - dates that version's section in `CHANGELOG.md`, for example `## [0.2.0] - 2026-11-02`.

   CI builds every release file for it, and it merges when green.
2. Tag the merged commit. Use `TZ=UTC`, because the tag records the time and zone:

   ```bash
   TZ=UTC git tag -a v0.2.0 -m "sshuttle2 0.2.0" COMMIT
   ```

   ```bash
   git push origin v0.2.0
   ```

3. The Release workflow takes about 15 minutes. If the tag and `Cargo.toml` disagree, or the CHANGELOG section has no date, it stops and publishes nothing.

**Prerelease:** version `0.2.0-rc.1`, tag `v0.2.0-rc.1`. It uses the `0.2.0` section of the CHANGELOG, and GitHub marks it as a prerelease.

**If publishing fails halfway:** rerun the failed jobs. The publish step finds its draft and fills it in. It never changes a published release. To fix a bad release, publish a new version.

**Try it without a tag:** Actions → Release build → Run workflow, on any branch. It builds and tests everything and publishes nothing. Pull requests that touch the release build run it as well.

## What a release contains

| File | What |
|---|---|
| `sshuttle2-VERSION-{macos,linux}-{x86_64,arm64}.tar.gz` | The client, licenses and README. The Linux clients are static (musl) and run on any distribution. |
| `sshuttle2_VERSION_{amd64,arm64}.deb` | Debian and Ubuntu package |
| `sshuttle2-VERSION-1.{x86_64,aarch64}.rpm` | Fedora, RHEL and compatible package |
| `sshuttle2-VERSION-src.tar.gz` | The source, as `git archive` of the tag |
| `install.sh` | Downloads and checks the right tarball |
| `SHA256SUMS` | SHA-256 of all of the above |

Each file also has a build provenance attestation. Check one with `gh attestation verify FILE --repo allwinpower/sshuttle2`.

Build the same files locally with `cargo xtask servers`, then `cargo xtask client TARGET` and `cargo xtask package TARGET` for each target. Linux packages need [nfpm](https://nfpm.goreleaser.com/).
