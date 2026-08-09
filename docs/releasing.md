# Releasing Urmare

GitHub Releases are the canonical source for Urmare's prebuilt binaries. A
release starts from a stable semantic-version tag such as `v0.1.0` and ends as
a draft GitHub Release that a maintainer reviews and publishes.

## Version source

The release version is declared once in `[workspace.package].version` in the
root `Cargo.toml`. Every workspace crate inherits it. Clap's `version` setting
then compiles the `urmare-cli` package version into `urmare --version`.

The release workflow verifies all three observable values before publishing a
draft:

1. the Git tag without its leading `v`
2. the root Cargo workspace version
3. the version reported by every built binary

## Create a release

Before tagging, merge the version change to `main` and wait for the required CI
check to pass on that exact commit. Then create and push an annotated tag:

```bash
git switch main
git pull --ff-only
git tag -a v0.1.0 -m "Urmare v0.1.0"
git push origin v0.1.0
```

The tag must be a stable `vMAJOR.MINOR.PATCH` version, must match the workspace
version, and must point to a commit on `main`. Repository rules should restrict
who can create or update release tags.

## What the workflow builds

`.github/workflows/release.yml` builds and smoke-tests optimized binaries for:

- macOS ARM64 and x86-64
- Linux glibc ARM64 and x86-64
- Windows MSVC x86-64

Each platform job packages the binary with `README.md` and `LICENSE`. The final
job checks that the complete artifact set exists, writes `SHA256SUMS`, creates
GitHub artifact attestations, and creates a draft GitHub Release with generated
notes. A rerun may replace assets on an existing draft, but it refuses to alter
an already-published release.

The Linux glibc binaries are built natively on Ubuntu 22.04. Linux musl builds,
macOS signing and notarization, Windows code signing, PyPI, crates.io, Homebrew,
and installer publishing are later release slices.

## Review and publish

Before publishing the draft:

1. inspect the generated release notes and the complete artifact list
2. verify the checksums from the directory containing the downloaded assets
3. verify at least one artifact's GitHub attestation
4. extract representative archives and run `urmare --version`

For example:

```bash
# Linux
sha256sum --check SHA256SUMS

# macOS
shasum -a 256 --check SHA256SUMS

gh attestation verify urmare-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo sorginte/urmare
```

Publish the GitHub draft only after those checks pass. Published release tags
and assets should be treated as immutable; issue a new patch version instead of
reusing a published tag.
