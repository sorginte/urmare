# GitHub Actions validation planning

The planning Action is implemented for the **intended v0.5.0 release**. That
tag and its release archives do not exist yet. Examples using
`uses: sorginte/urmare@v0.5.0` describe usage after publication, not an available
release today. Until then, source-build CI uses `uses: ./` and the `binary`
override with the candidate's actual version (currently 0.4.0).

## Planning and execution

The Action installs a pinned standalone Urmare binary, then independently runs:

```text
urmare --root ROOT plan --git-diff BASE --json
```

It does not accept an earlier plan from an agent, PR description, or artifact.
Analysis stays in the CLI/shared application layer. This preserves pytest-aware
dependencies, exclusions, merge-base comparison, deleted-file identities,
previous rename identities, and conservative fallbacks.

The Action never executes pytest, installs project dependencies, or accepts
arbitrary commands. The consuming workflow owns its Python environment, runs
the validated steps, and uploads artifacts. No hosted Sorginte service, write
token, PR-comment permission, or secret is needed.

## Inputs and outputs

| Input | Default | Meaning |
| --- | --- | --- |
| `root` | `GITHUB_WORKSPACE` | Existing Git top-level directory. Relative inputs resolve from the workspace; absolute paths are accepted. |
| `base` | **Required**, no event-derived default | Existing local Git commit/ref. Prefer the PR's recorded `base.sha`. Empty and option-like values fail. Non-PR events must also provide a base. |
| `version` | **Required**, no default | Exact stable `MAJOR.MINOR.PATCH`, without `v`. Required with either installation path. No prereleases, ranges, or `latest`. |
| `plan-path` | Unique `RUNNER_TEMP/urmare-plan-UUID.json` | New absolute output path outside the canonical analyzed root. Its parent must exist. Existing files and symlinks are refused, never overwritten. |
| `binary` | Empty: download the release | Explicit existing executable path, relative to the workspace or absolute. Bypasses all downloads and platform selection, but must report the exact requested version. |

Explicit inputs take precedence over defaults. An empty `root`, `plan-path`, or
`binary` selects its default. `base` and `version` never fall back to event
metadata, PATH binaries, the Action revision, or the Cargo manifest. Roots,
paths, and bases cannot contain control characters. `RUNNER_TEMP` must be outside
the analyzed root. Canonicalization catches symlink parents into that root.

| Output | Meaning |
| --- | --- |
| `mode` | `selective`, `none`, or `full` |
| `plan-path` | Absolute path of the complete schema-version-1 CLI JSON |
| `affected-test-count` | Decimal length of `affected_tests`; not a restriction on full discovery |

Outputs are usable only after the Action step succeeds. No shell-joined pytest
arguments are exposed. The JSON is preserved byte for byte, with no additional
fields or truncation. It is staged in an Action-owned temporary sibling and
published atomically with a no-clobber hard link on the same filesystem. Invalid,
partial, interrupted, or failed planning never publishes a partial plan. Use a
new path per invocation; do not reuse a stale artifact after failure.

The job summary shows the analyzed HEAD, supplied base, validation mode,
changed Python-file count, affected test-file count, full-validation reason
when present, and complete plan path. Configuration-only changes can have zero
changed Python files and still require full validation. Paths and values are
escaped for Markdown/HTML and GitHub output files. CLI diagnostics remain in
the planning step's stderr log, quoted to prevent workflow-command injection.

## Binary provenance and runners

The Action revision pins integration code; `version` independently pins the
analysis executable. For production, pin Actions to reviewed full commit SHAs
and review updates to both pins. The public release entry point will be
`sorginte/urmare@v0.5.0`; replace it with that release's immutable commit SHA
after publication. There is intentionally no placeholder release SHA here.

Downloads use only:

```text
https://github.com/sorginte/urmare/releases/download/vVERSION/SHA256SUMS
https://github.com/sorginte/urmare/releases/download/vVERSION/urmare-vVERSION-TARGET.ARCHIVE
```

The version is validated before URL construction. The selected archive must
match exactly one SHA-256 entry in the release's `SHA256SUMS`. All archive
members are checked for unsafe paths, duplicates, links, and special files;
only the regular executable is copied into a unique Action-owned directory
under `RUNNER_TEMP`. It must report `urmare VERSION` before planning. Nothing
is installed globally or added to PATH. Temporary downloads and binaries are
removed after the invocation; Urmare may update its private OS analysis cache.

Supported downloads match the existing standalone release matrix:

| Runner | Archive target |
| --- | --- |
| Linux glibc x86-64 | `x86_64-unknown-linux-gnu` |
| Linux glibc ARM64 | `aarch64-unknown-linux-gnu` |
| macOS x86-64 | `x86_64-apple-darwin` |
| macOS ARM64 | `aarch64-apple-darwin` |
| Windows x86-64 | `x86_64-pc-windows-msvc` |

Unix archives are `.tar.gz`; Windows uses `.zip`. Linux binaries retain the
release's glibc 2.17 baseline. Musl, Windows ARM64, and other platforms have no
published archive in this slice and fail explicitly. A caller-provided native
binary can bypass download platform selection. Checksum verification detects
corruption against the release checksum file; it is not independent signature
or attestation verification. See [releasing.md](releasing.md) for the release's
build-once/package-twice provenance and optional attestation verification.

Git, Bash, and Python 3.9+ as `python` must already be available on the runner.
GitHub-hosted native runners provide these. Self-hosted runners must supply
them, executable permissions, outbound HTTPS for downloads (unless overriding
the binary), and a temporary filesystem supporting hard links. Rust is needed
only when the caller chooses to build a candidate binary.

## Complete PR workflow

For a uv project with an existing lockfile and a pytest dependency, copy
[`examples/ci/run_pytest.py`](../examples/ci/run_pytest.py) into your repository
as `.github/scripts/urmare_pytest.py`, then copy this workflow into
`.github/workflows/python-validation.yml`. The script is self-contained and
uses only Python's standard library. Keep it reviewed alongside your workflow.
Adapt `uv sync --locked` to the project's established dependency groups.

This workflow targets the intended, not-yet-published v0.5.0. After that release,
replace its tag reference with the release's reviewed full commit SHA before
enabling it under an immutable-pinning policy. All currently available external
Actions below use immutable revisions.

```yaml
name: Python validation
on: [pull_request]
permissions:
  contents: read
jobs:
  pytest:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
        with:
          ref: ${{ github.event.pull_request.head.sha }}
          fetch-depth: 0
          persist-credentials: false
      - name: Ensure the recorded base commit is available
        shell: python
        env:
          BASE_SHA: ${{ github.event.pull_request.base.sha }}
        run: |
          import os, re, subprocess
          base = os.environ['BASE_SHA']
          if not re.fullmatch(r'[0-9a-f]{40}', base):
              raise SystemExit('Expected the recorded PR base commit SHA')
          subprocess.run(['git', 'fetch', '--no-tags', '--no-recurse-submodules',
                          'origin', base], check=True)
      - uses: actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97 # v7.0.0
        with:
          python-version: '3.14'
      - uses: astral-sh/setup-uv@20cfd1bf945f4377ade1205e4dbc17946fc9a30d # v10.0.1
        with:
          version: '0.12.5'
          enable-cache: false
      - name: Prepare the existing project environment
        run: uv sync --locked
      - name: Independently compute the validation plan
        id: plan
        uses: sorginte/urmare@v0.5.0 # Intended release; pin its full SHA after publication
        with:
          version: '0.5.0'
          base: ${{ github.event.pull_request.base.sha }}
          plan-path: ${{ runner.temp }}/urmare-plan.json
      - name: Execute every required pytest step
        shell: python
        env:
          PLAN_PATH: ${{ steps.plan.outputs.plan-path }}
        run: |
          import os, subprocess, sys
          subprocess.run([sys.executable, '.github/scripts/urmare_pytest.py',
                          os.environ['PLAN_PATH'], os.environ['GITHUB_WORKSPACE']], check=True)
      - name: Preserve the complete plan even when pytest fails
        if: ${{ always() }}
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: urmare-validation-plan
          path: ${{ runner.temp }}/urmare-plan.json
          if-no-files-found: warn
          retention-days: 14
```

Planning and validation use the same checkout, without another checkout/reset
between them. The selected commit is the PR's recorded **head SHA**, including
fork PRs. GitHub's ordinary `pull_request` default checkout instead analyzes a
synthetic merge commit; this example explicitly chooses the head. If your
policy requires testing the merge result, use the default merge checkout for
both planning and validation, retain the recorded base SHA, and inspect the
summary's actual HEAD. See GitHub's [pull-request event semantics](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request).

`base.sha` is the PR event's recorded comparison commit, not a guessed branch
name or a moving `origin/main`. Full history is required for the CLI's merge
base. The explicit validated-SHA fetch covers bases no longer reachable from
fetched branch refs; fetch failure fails the job. The Action and core do not
fetch anything themselves. A ref is accepted as `base` for other uses, but a
recorded commit SHA is more reproducible. Missing objects, unrelated histories,
invalid bases, and shallow clones fail with Git/history diagnostics. The Action
does not substitute a two-dot diff or a successful `none` result.

Use ordinary `pull_request`, never `pull_request_target` to check out or execute
PR code. Forks need no secrets or write permissions. Pytest and environment
setup execute repository code under the workflow's normal untrusted-PR trust
boundary. Do not attach credentials to that execution. Keep normal fork-run
approval policies. The Action itself accepts no executable commands other than
the explicit trusted Urmare binary override.

## Consuming the plan safely

The vendored consumer validates the whole JSON before executing anything:

- schema version 1, required fields, and known modes;
- supported pytest step kind and literal program `pytest`;
- string argument arrays and consistent steps/affected tests;
- full-validation metadata with a known reason;
- normalized relative paths, existing Python test files, and resolved root
  containment, including symlinks.

It rejects absolute paths, parent traversal, backslashes, drive paths, control
characters, and pytest node IDs. Spaces and shell metacharacters remain single
arguments. It constructs the literal prefix `['uv', 'run', 'pytest']` itself,
never executes the JSON `program`, and uses `subprocess.run` without a shell.
Selective targets follow `--` and receive `./` prefixes, so a filename starting
with `-` cannot become a pytest option. No eval, command substitution, shell
interpolation, or shell-joined targets are involved.

| Mode | Consumer behavior |
| --- | --- |
| `selective` | Runs every selected file; validates all steps first and attempts each returned step, retaining nonzero status if any fails. Current Urmare emits one step. |
| `full` | Runs `uv run pytest` with no selected-file restriction, even if `affected_tests` is empty. |
| `none` | Prints an explicit skip decision and invokes no pytest process. |
| Invalid/inconsistent plan | Fails before any pytest execution. |
| Planning or validation error | Fails the job; never converts failure to `none`. |

Full-validation reasons are `configuration_changed`,
`unresolved_dynamic_import`, `unsupported_pytest_relationship`, and
`unsupported_exports`. Inspect the unchanged `full_validation` object, including
its historical `configuration_paths` field, for the cause. These semantics
come from the CLI, not Action heuristics.

For another established pytest environment, edit the consumer's literal prefix:
for example `['poetry', 'run', 'pytest']` or `[sys.executable, '-m', 'pytest']`.
Review that source change in your repository; the Action has no command-prefix
input and Urmare gains no execution command. Keep the validation and option
boundary intact. Repository pytest configuration and plugins remain responsible
for their normal runtime behavior. Pytest's own nonzero statuses, including
collection failures or no tests collected during full discovery, fail the job.

Artifact upload runs separately with `always()`. A validation failure still
preserves the successful plan; planning failure leaves no new plan, so upload
warns about the missing file and the earlier failure remains a failure. Step
logs retain CLI diagnostics and pytest stdout/stderr. The summary identifies
the exact checkout and points at the JSON; the artifact is the complete plan,
not a substitute for independently planning another run.

## Adoption, troubleshooting, and limitations

Start by retaining the existing required full-suite job on the same head SHA.
Run this planning/consumer job alongside it and collect artifacts over typical
PRs. Compare selective successes with full-suite failures and investigate
omitted tests with `urmare why`, especially fixture/plugin/dynamic patterns.
Keep periodic full-suite validation and full validation for changes your own
project policy requires. Static analysis cannot identify every runtime
dependency; unselected tests are not universally safe to skip. Non-Python
inputs such as data files, lockfiles, and CI configuration may need additional
full-suite policy outside this first integration.

- **Release download fails:** confirm an actual published stable version and
  supported target; do not request v0.5.0 before publication. For candidate CI,
  build `cargo build --release --locked -p urmare-cli`, supply `binary`, and use
  the candidate's actual `--version`, not the intended release version.
- **Checksum or reported version mismatch:** fail and inspect the binary source;
  do not disable verification. Override executables are caller-trusted code.
- **Base/history failure:** check the recorded SHA, fetch it in the workflow,
  and use `fetch-depth: 0`; unrelated or unavailable histories cannot be guessed.
- **Output path failure:** choose a new absolute file beneath `runner.temp`
  with an existing parent, outside the root. Hard links must be supported.
- **Plan rejected:** inspect the full JSON and pinned binary/consumer versions;
  do not fall back to a skip. This consumer supports only the current v1 pytest
  structure and portable Python file targets.
- **Unexpected full validation:** inspect `full_validation.reason`; full is an
  intentional conservative result, not a planning failure.

Keep the checkout unchanged between planning and execution. The CLI also
includes working-tree changes; the Action reports HEAD and detects a changed
HEAD during planning, but it does not lock the worktree or attest runtime
environment equivalence. Argument length limits remain those of the runner OS;
this first consumer does not batch a very large step or add pytest plugins.

Urmare's own CI uses a purpose-built Python Git fixture and a controlled pytest
subprocess harness, while preserving all existing Rust, portability, packaging,
and release checks. Normal installer/consumer tests are offline and construct
archives locally. The candidate Action never needs an unpublished archive.
Other CI adapters, arbitrary validation configuration, PR comments/labels,
write-API checks, agent enforcement, and hosted services remain separate work.
