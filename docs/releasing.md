# Releasing Urmare

Urmare has two binary distribution channels. GitHub Releases contain standalone
archives, while PyPI contains platform-specific wheels that install the same
binary as the `urmare` command. Neither channel contains a source distribution.

The release invariant is:

> Build once per target, package twice, attach only standalone archives to
> GitHub, and promote the exact wheels produced by those builds to PyPI.

## Version and artifact identity

The release version is declared once in `[workspace.package].version` in the
root `Cargo.toml`. Workspace crates inherit it, Maturin obtains the Python
package version dynamically from `urmare-cli`, and Clap compiles that package
version into `urmare --version`.

For a release, automation requires exact agreement between:

1. the stable `vMAJOR.MINOR.PATCH` Git tag;
2. the root Cargo workspace version;
3. the wheel filename and wheel `METADATA`;
4. the installed wheel's `urmare --version`; and
5. every standalone archive's `urmare --version`.

The tag must point to a commit on `main`. Each target manifest records the tag,
version, full commit SHA, target, wheel and archive filenames and SHA-256
digests, plus the packaged binary's SHA-256 digest.

## Build-once/package-twice workflow

`.github/workflows/build-binaries.yml` is the reusable builder. It runs through
`workflow_call` during a release and through path-filtered pull requests or
manual dispatch for safe packaging validation. Those non-release paths upload
temporary Actions artifacts only: they cannot create a release, request PyPI
OIDC credentials, publish a package, or create a tag.

For each target, `maturin build` performs the optimized Cargo build once and
creates one interpreter-independent binary wheel. The packaging helper reads
the executable directly from that wheel and writes the standalone archive.
It then compares the SHA-256 digest of the wheel executable with the executable
inside the archive and fails if they differ. There is no second Cargo release
build for the archive.

The target and Actions artifact channels are:

| Target | Wheel filename | Standalone archive | Actions channels |
| --- | --- | --- | --- |
| `aarch64-apple-darwin` | `urmare-VERSION-py3-none-macosx_*_arm64.whl` | `urmare-vVERSION-aarch64-apple-darwin.tar.gz` | `wheels-aarch64-apple-darwin`, `archives-aarch64-apple-darwin` |
| `x86_64-apple-darwin` | `urmare-VERSION-py3-none-macosx_*_x86_64.whl` | `urmare-vVERSION-x86_64-apple-darwin.tar.gz` | `wheels-x86_64-apple-darwin`, `archives-x86_64-apple-darwin` |
| `aarch64-unknown-linux-gnu` | `urmare-VERSION-py3-none-manylinux_2_17_aarch64*.whl` | `urmare-vVERSION-aarch64-unknown-linux-gnu.tar.gz` | `wheels-aarch64-unknown-linux-gnu`, `archives-aarch64-unknown-linux-gnu` |
| `x86_64-unknown-linux-gnu` | `urmare-VERSION-py3-none-manylinux_2_17_x86_64*.whl` | `urmare-vVERSION-x86_64-unknown-linux-gnu.tar.gz` | `wheels-x86_64-unknown-linux-gnu`, `archives-x86_64-unknown-linux-gnu` |
| `x86_64-pc-windows-msvc` | `urmare-VERSION-py3-none-win_amd64.whl` | `urmare-vVERSION-x86_64-pc-windows-msvc.zip` | `wheels-x86_64-pc-windows-msvc`, `archives-x86_64-pc-windows-msvc` |

Linux wheels and archives use the same `manylinux_2_17`-compatible executable.
Each `archives-*` Actions artifact wraps exactly one final `.tar.gz` or `.zip`;
each `wheels-*` artifact wraps exactly one `.whl`. GitHub's artifact wrapper
does not modify the inner file. Per-target `manifest-part-*` artifacts feed an
internal `release-manifest` audit artifact, which is retained separately and
is not a public release asset.

The reusable workflow smoke-tests both the archive and an isolated wheel
installation on every native target, verifies Python 3.9 and 3.14 on Linux
x86-64, and exercises pip, pipx, `uv tool`, uvx, and `pipx run`. The final job
requires exactly five wheels, five archives, and five target manifests; it
rejects missing, duplicate, unexpected, or source-distribution files.

## Release orchestration

`.github/workflows/release.yml` is the top-level, tag-driven release workflow:

1. validate the tag, Cargo version, full commit SHA, and ancestry on `main`;
2. call the reusable five-target builder;
3. verify the complete archive and wheel set and retain the internal manifest;
4. create GitHub build-provenance attestations for the wheels;
5. download only `archives-*`, generate and verify `SHA256SUMS`, and attest the
   public assets;
6. create or update a draft GitHub Release using generated release notes and
   upload only the five archives and `SHA256SUMS`;
7. wait at the protected `pypi` GitHub environment; and
8. after approval, download only `wheels-*` from the same workflow run and
   publish them with PyPI Trusted Publishing.

The PyPI job has only two steps: artifact download and the official pinned PyPI
publisher. It does not check out source, compile code, accept a long-lived PyPI
token, publish an sdist, or use `skip-existing`. Trusted Publishing supplies a
short-lived OIDC credential and produces PyPI publish attestations. The GitHub
Release remains a draft after PyPI succeeds so a maintainer can perform the
final public-release review. If PyPI fails, the release remains a draft.

The public GitHub Release contains only:

- four Unix `.tar.gz` archives;
- one Windows `.zip` archive; and
- `SHA256SUMS`.

Wheels, Actions artifact wrappers, and internal manifests are never attached.
PyPI receives only the five verified wheels from that same workflow run.

## One-time manual configuration

Configure these settings before the first PyPI release:

1. In GitHub, create an environment named `pypi`. Add required reviewers and
   restrict deployment to the protected stable release tags used by this
   repository. Do not add a PyPI API token or other publishing secret.
2. In PyPI, register a Trusted Publisher for owner `sorginte`, repository
   `urmare`, workflow `release.yml`, and environment `pypi`. For a project that
   does not yet exist, use PyPI's pending-publisher flow; otherwise configure it
   in the project's Publishing settings.
3. Protect `v*.*.*` tags with repository rules that restrict creation, update,
   and deletion to release maintainers. Protect `main` with the required CI
   check and pull-request review policy.
4. Ensure GitHub artifact attestations are available for the repository. The
   workflow intentionally fails rather than silently dropping provenance.
5. Create and maintain the release-note labels listed below.

The repository does not currently contain Dependabot or Renovate configuration.
Maintaining immutable Action SHA pins through one of those tools is a focused
follow-up; update pins manually until then.

## Release notes

`.github/release.yml` configures GitHub's native generated notes. `gh release
create --generate-notes` includes merged pull-request links, contributors, and
the full comparison link, grouped in this order:

1. `breaking` — Breaking changes
2. `feature`, `enhancement` — Features
3. `bug` — Fixes
4. `performance` — Performance
5. `documentation` — Documentation
6. `packaging`, `ci` — Packaging and CI
7. `dependencies` — Dependencies
8. everything else — Other changes

Pull requests labeled `skip-changelog` are excluded. Maintainers assign
semantic labels such as `breaking`, `feature`, and `bug`; they are not inferred
from file paths.

## Create and approve a release

Before tagging, merge the version change to `main` and wait for required CI on
that exact commit. Then create and push an annotated tag:

```bash
git switch main
git pull --ff-only
git tag -a v0.2.0 -m "Urmare v0.2.0"
git push origin v0.2.0
```

The workflow creates the draft before requesting approval at the `pypi`
environment. The reviewer should inspect its generated notes, five archives,
and `SHA256SUMS`, then approve PyPI promotion. After the workflow succeeds:

1. verify the PyPI files and provenance;
2. smoke-test one PyPI installation and one standalone archive; and
3. manually publish the existing GitHub draft.

## Checksums and provenance

Verify downloaded GitHub assets from the directory containing them:

```bash
# Linux
sha256sum --check SHA256SUMS

# macOS
shasum -a 256 --check SHA256SUMS

gh attestation verify urmare-v0.2.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo sorginte/urmare
gh attestation verify SHA256SUMS --repo sorginte/urmare
```

Wheels receive GitHub build-provenance attestations before publication and
PyPI publish attestations during Trusted Publishing. After downloading the
wheel selected for the current platform, verify its GitHub provenance with:

```bash
gh attestation verify urmare-0.2.0-py3-none-PLATFORM.whl \
  --repo sorginte/urmare
```

PyPI also exposes each distribution's publish provenance on the project file
details page. Its official verifier accepts the PyPI file URL shown there:

```bash
pypi-attestations verify pypi \
  --repository https://github.com/sorginte/urmare \
  "$WHEEL_DIRECT_URL"
```

## Failure, rerun, and immutability policy

- Pull requests and manual runs of the reusable builder are non-publishing dry
  runs and are the normal way to test packaging changes before tagging.
- A failed tag workflow can be rerun. Existing draft assets may be replaced by
  newly validated artifacts built from the same immutable tag commit.
- The release workflow refuses to replace assets on an already-published
  GitHub Release.
- PyPI files are immutable. The publisher does not skip existing files. If a
  filename conflicts and byte identity cannot be proven safely, make the fix
  and issue a new patch version instead of overwriting or ignoring it.
- Published Git tags, GitHub assets, and PyPI files are immutable by project
  policy. Never move or recreate a published tag.
- If PyPI succeeds but a later manual review finds a problem, keep or withdraw
  the affected project release as appropriate and publish a corrected patch;
  do not reuse the version.

macOS notarization, Windows signing, musllinux, Windows ARM64, Homebrew,
crates.io, installers, and source distributions are outside this release slice.
