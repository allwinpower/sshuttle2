# Releasing

A version tag publishes a release. [release.yml](../.github/workflows/release.yml) runs [build-release.yml](../.github/workflows/build-release.yml), which builds every file. It installs them on several Linux distributions, and installs the Homebrew formula on macOS and Linux. Then release.yml publishes the GitHub Release. For a final release, it also commits the new formula to [allwinpower/homebrew-tap](https://github.com/allwinpower/homebrew-tap).

## Steps

1. Open a pull request that:
   - sets `version` under `[workspace.package]` in `Cargo.toml`, then runs `cargo update -w` to update `Cargo.lock`;
   - renames `## [Unreleased]` in `CHANGELOG.md` to the version with a UTC date, for example `## [0.2.0] - 2026-11-02`, adds a new empty `## [Unreleased]` above it, and updates the compare links at the bottom.

   CI builds every release file for it, and it merges when green.
2. Tag the merged commit. Use `TZ=UTC`, because the tag records the time and zone:

   ```bash
   TZ=UTC git tag -a v0.2.0 -m "sshuttle2 0.2.0" COMMIT
   ```

   ```bash
   git push origin v0.2.0
   ```

3. The Release workflow takes about 15 minutes. If the tag and `Cargo.toml` disagree, or the CHANGELOG section has no date, it stops and publishes nothing.

**Prerelease:** version `0.2.0-rc.1`, tag `v0.2.0-rc.1`. It uses the `0.2.0` section of the CHANGELOG, and GitHub marks it as a prerelease. Homebrew does not get it.

**Homebrew token:** the release job pushes the formula with the `HOMEBREW_TAP_TOKEN` secret. That is a fine-grained token with contents read and write on `allwinpower/homebrew-tap` only. When it expires, create a new one, store it with `gh secret set HOMEBREW_TAP_TOKEN -R allwinpower/sshuttle2`, and rerun the failed `homebrew` job.

**AUR (Arch Linux):** after a final release, run:

```bash
gh workflow run aur.yml -f version=0.2.0
```

It builds the `sshuttle2-bin` package from the published release on Arch Linux, installs and checks it, and pushes it to the AUR with the `AUR_SSH_KEY` secret. That secret is the private half of an SSH key whose public half is in the AUR account. The release build's `aur-test` job already builds the package before every release, so the workflow should not fail on a good release.

**If publishing fails halfway:** rerun the failed jobs. The publish step finds its draft and fills it in. It never changes a published release. To fix a bad release, publish a new version.

**Try it without a tag:** Actions → Release build → Run workflow, on any branch. It builds and tests everything and publishes nothing. Pull requests that touch the release build run it as well.

## What a release contains

| File | What |
|---|---|
| `sshuttle2-VERSION-{macos,linux}-{amd64,arm64}.tar.gz` | The client, licenses and README. `amd64` is x86_64 (Homebrew would read `-x86_64` in a prerelease as version 86.64). The Linux clients are static (musl) and run on any distribution. |
| `sshuttle2_VERSION_{amd64,arm64}.deb` | Debian and Ubuntu package |
| `sshuttle2-VERSION-1.{x86_64,aarch64}.rpm` | Fedora, RHEL and compatible package |
| `sshuttle2-VERSION-1-{x86_64,aarch64}.pkg.tar.zst` | Arch Linux package, for `pacman -U`. Final releases only: nfpm would drop a prerelease from its version. |
| `sshuttle2-VERSION-src.tar.gz` | The source, as `git archive` of the tag |
| `install.sh` | Downloads and checks the right tarball |
| `SHA256SUMS` | SHA-256 of all of the above |

Each file also has a build provenance attestation. Check one with `gh attestation verify FILE --repo allwinpower/sshuttle2`.

Build the same files locally with `cargo xtask servers`, then `cargo xtask client TARGET` and `cargo xtask package TARGET` for each target. Linux packages need [nfpm](https://nfpm.goreleaser.com/).
