# Urmare

Urmare is Sorginte's high-performance dependency and impact-analysis engine for
Python repositories. Its core question is:

> Given a Python code change, what follows from it?

The project is an early MVP. It builds a deterministic local import graph,
calculates file-level blast radius, selects affected pytest files, and explains
why a dependent is affected. Core analysis runs locally and does not require a
hosted service or a Python interpreter.

## Current capabilities

- discover `.py` files while ignoring Git metadata, virtual environments, and
  common Python tool caches
- infer flat and repository-root `src/` layouts
- load optional source roots, test roots, and exclusions from `[tool.urmare]`
- resolve imports across multiple explicitly configured source roots
- map repository-relative paths to Python modules
- parse absolute and relative static imports
- resolve imports to repository-local modules and report located, structured
  unresolved imports
- retain every source import and location that creates a resolved local edge
- trace local-resolution candidates and matches for debugging
- inspect module mappings, dependency edges, and per-module neighbor counts
- calculate direct and transitive reverse dependencies
- union explicit multi-file impact with per-result attribution
- detect staged, unstaged, and untracked Python changes against `HEAD`, or add
  committed branch changes relative to an explicit Git merge base
- union impact across multiple Git-changed files with per-result attribution
- preserve conservative impact for deleted and renamed modules
- explain Git-selected changes, including deleted paths and previous rename identities
- require full validation and select every current test when root configuration changes
- emit stable, versioned JSON for every current command
- maintain a versioned persistent repository index and update bounded Git
  change sets without reconstructing the complete graph
- query persistent forward/reverse relationships directly for impact and
  explanations, with measured index, update, persistence, and query work
- discover `test_*.py` and `*_test.py` files plus configured test trees
- select affected test files
- return one deterministic shortest dependency path with import evidence for
  every hop
- emit actionable errors for invalid input, configuration, and Python syntax

Urmare currently analyzes static imports only. It does not claim symbol-level,
call-graph, runtime, or framework-specific semantic understanding.

## Install

PyPI provides platform-specific binary wheels for Python 3.9 through 3.14. The
wheels install the standalone Rust CLI as the `urmare` command; they do not
compile code and do not require Rust, Cargo, a C compiler, or a CPython ABI.

Install Urmare as an isolated tool with uv or pipx:

```bash
uv tool install urmare
urmare impact src/example.py
```

```bash
pipx install urmare
urmare impact src/example.py
```

Or install it into a virtual environment with pip:

```bash
python -m venv .venv
source .venv/bin/activate
pip install urmare
urmare impact src/example.py
```

On Windows, activate the environment with `.venv\Scripts\activate`.
Temporary execution also installs a prebuilt wheel:

```bash
uvx urmare impact src/example.py
pipx run urmare impact src/example.py
```

The binary wheels support macOS ARM64 and x86-64, Linux glibc ARM64 and x86-64
with a `manylinux_2_17` baseline, and Windows MSVC x86-64. GitHub Releases are
the standalone-archive channel for the same five binaries; PyPI is the
Python-tool installation channel. Wheels are deliberately not attached to
GitHub Releases.

## Build from source

Contributors can build the Rust workspace with Rust 1.95 or newer:

```bash
git clone https://github.com/sorginte/urmare.git
cd urmare
cargo build --release
```

The CLI binary is written to `target/release/urmare` (`urmare.exe` on Windows).
See the [release process](docs/releasing.md) for artifact identity, integrity
checks, provenance, and the maintainer procedure.

For development, commands can be run without installing the binary:

```bash
cargo run -p urmare-cli -- graph
```

## Usage

Use the top-level help to discover commands and command-specific help to inspect
their arguments and options:

```bash
urmare --help
urmare help graph
```

Run Urmare from the repository being analyzed:

```bash
urmare graph
urmare graph --all
urmare graph --json
urmare graph --debug
urmare graph --debug --focus src/example/service.py
urmare graph --debug --json
urmare impact src/example/service.py
urmare impact src/example/service.py src/example/models.py
urmare impact src/example/service.py --all
urmare impact --changed
urmare impact --changed --json
urmare impact --git-diff main
urmare impact --git-diff main --json
urmare tests --affected src/example/service.py
urmare tests --affected src/example/service.py src/example/models.py
urmare tests --affected --changed
urmare tests --affected --changed --json
urmare tests --affected --git-diff main
urmare tests --affected --git-diff main --json
urmare why src/example/service.py tests/test_api.py
urmare why src/example/service.py tests/test_api.py --json
urmare why src/example/service.py tests/test_api.py --changed
urmare why src/example/service.py tests/test_api.py --git-diff main --json
```

The complete command grammar is:

```text
urmare [--root PATH] graph [--json|--all] [--debug [--focus FILE]]
urmare [--root PATH] impact <FILE...|--changed|--git-diff BASE> [--json|--all]
urmare [--root PATH] tests --affected <FILE...|--changed|--git-diff BASE> [--json]
urmare [--root PATH] why CHANGED_FILE AFFECTED_FILE [--changed|--git-diff BASE] [--json]
```

The alternative change sources are mutually exclusive.

Or select a repository explicitly:

```bash
urmare --root path/to/repository graph
```

`graph` reports unmatched static imports with one-indexed target locations:

```text
Unresolved import details (2)
  No repository-local module matched; external packages are not resolved.
  src/api/app.py:4:8  import fastapi
  tests/test_app.py:1:20  from pytest import fixture
```

These entries are diagnostics, not claims that an import is invalid: Urmare
does not inspect installed environments or package indexes. Human output shows
at most 25 entries by default; `graph --all` displays every entry and
`graph --json` always contains the complete unresolved-import list.

Use `graph --debug` when a module mapping or impact result is surprising. It
adds the inferred source roots, repository path-to-module mappings, resolved
edges with their source imports, and a trace for every local-resolution
attempt. Each trace distinguishes resolved imports, unmatched imports, and
relative imports that ascend above the importer package; it lists every dotted
module candidate considered and every repository path matched.

The following is an abbreviated inspection:

```text
Graph inspection

Module mappings (1)
  src/payments/service.py -> payments.service [source; 2 dependencies; 1 dependents]

Resolved import edges (3)
  src/payments/service.py -> src/payments/stripe.py
    via src/payments/service.py:1:15  from . import stripe
  ...

Import resolution trace (1)
  src/payments/service.py:1:15  from . import stripe [resolved]
    candidates: payments, payments.stripe
    matched payments -> src/payments/__init__.py
    matched payments.stripe -> src/payments/stripe.py
```

`--focus <file>` requires `--debug` and restricts module mappings and import
attempts to that file while retaining both incoming and outgoing incident
edges. Debug human output is bounded to 25 modules, edges, and traces per
section unless `--all` is passed. `graph --debug --json` is always complete;
plain `graph --json` preserves the concise summary schema.

An impact result starts with counts and then lists directly affected modules,
transitively affected modules, and affected tests in separate sections. Test
files are shown in the test section rather than duplicated as modules. Human
output shows at most 25 entries per section by default and reports how many were
omitted; pass `--all` to display every entry. `--json` is always complete and is
never subject to the human-output limit.

```text
Impact analysis

Changed (1)
  src/payments/stripe.py

Summary
  Directly affected modules      2
  Transitively affected modules  1
  Affected tests                  2

Directly affected modules (2)
  src/payments/formatters/card.py
  src/payments/service.py

Transitively affected modules (1)
  src/api/checkout.py

Affected tests (2)
  tests/api/test_checkout.py
  tests/payments/test_stripe.py
```

Urmare does not assign `LOW`, `MEDIUM`, or `HIGH` blast-radius/risk labels. A
risk classification will require a separately specified deterministic model.
`tests --affected` continues to print one canonical repository-relative test
path per line, making its output easy to compose with other tools.

Explicit path commands accept one or more changed Python files. Urmare
normalizes and deduplicates the inputs, unions their reverse dependency
closures, and records every changed file that caused each affected result. If
any input is missing, outside the repository, or not indexed, the command fails
with an actionable diagnostic instead of returning a partial result. Explicit
files, `--changed`, and `--git-diff` are mutually exclusive.

`--changed` analyzes the current Git working tree against `HEAD`. It includes
staged, unstaged, and untracked Python files that are not ignored by Git,
including added, deleted, and renamed paths. It does not include changes that
are already committed to `HEAD`:

```bash
urmare impact --changed
urmare impact --changed --json
urmare tests --affected --changed
urmare tests --affected --changed --json
```

`--git-diff <base>` additionally includes committed branch changes since the
merge base of the provided revision and `HEAD`, together with the same working
tree changes:

```bash
urmare impact --git-diff origin/main
urmare tests --affected --git-diff origin/main
urmare why src/payments/stripe.py tests/api/test_checkout.py --git-diff origin/main
```

Renames seed impact from both the old and new module identities. Deleted module
paths receive virtual graph nodes, allowing surviving imports and downstream
tests to remain connected without checking out the base revision. Deleted test
files themselves are not emitted as runnable affected tests.

Git-aware `why` requires its changed path to belong to the selected Git change
set. A deleted path and the previous path of a rename can be supplied even
though neither currently exists. The affected path must be a currently indexed
file. Successful explanations use the same deterministic shortest-path and
import-provenance contract as ordinary `why`.

Git-aware analysis requires the `git` executable. When `--root` is omitted,
`impact --changed`, `impact --git-diff`, `tests --affected --changed`, and
`tests --affected --git-diff`, plus Git-aware `why`, discover the containing
Git repository's top-level directory, so they work from a repository
subdirectory. An explicit `--root` remains authoritative and must identify the
Git top level.

## Configuration

Common flat and `src/` layouts remain zero-configuration. Repositories can
declare additional analysis boundaries in the selected root's
`pyproject.toml`:

```toml
[tool.urmare]
source-roots = ["packages/payments/src", "packages/api/src"]
test-roots = ["verification", "integration/checks"]
exclude = ["generated/**", "vendor", "**/snapshots/*.py"]
```

Configured source roots are authoritative for module mapping and are resolved
relative to the repository root. They must be non-empty repository-relative
directories and may not contain `..`. Files outside every configured root,
including conventional top-level tests, keep repository-relative module names.
If configured roots overlap, the most specific matching root wins.

File paths in results remain canonical and repository-relative; configuring a
source root changes `packages/payments/src/payments/service.py` into the
importable module `payments.service`, but output continues to show the complete
repository path. Urmare rejects configuration typos, missing roots, duplicate
roots, and module names exposed by more than one root instead of building an
ambiguous graph.

Configured test roots supplement filename discovery: every discovered `.py`
file beneath one of those roots is classified as a test, while conventional
`test_*.py` and `*_test.py` files elsewhere remain tests. Test roots affect
classification only; source roots remain the authority for module identity.

Exclusion patterns are repository-relative portable globs. Configuration must
use `/` separators on every platform; `*` stays within one path component and
`**` can span directories. A pattern matching a directory, such as `vendor`,
prunes the full subtree. Exclusions run before parsing and graph construction,
take precedence over source/test roots, and also filter Git-diff change seeds.
Renames crossing an exclusion boundary are conservatively treated as an add or
delete on the included side. Urmare's built-in ignores for `.git`, virtualenvs,
and common Python cache directories always remain active.

A Git change to the repository-root `pyproject.toml` can redefine all of these
boundaries. Added, modified, deleted, and renamed configuration therefore make
selective module impact unsafe. Git-aware `impact` reports `Full validation
required`, marks module impact unavailable, and selects every test currently
eligible under the current configuration. Git-aware `tests --affected` prints
all of those tests and writes an explicit warning to stderr in human mode. JSON
uses the `full_validation` object documented below. This also applies when
`pyproject.toml` is the only changed path, so configuration changes never look
like an empty selective result. Git-aware `why` returns an actionable
full-validation diagnostic in this state because a selective dependency
explanation may be invalid.

## Persistent incremental index

Normal analysis stores a versioned repository index in Urmare's
platform-standard per-user cache directory. Entries are isolated by canonical
repository root, and Urmare never writes index data into the analyzed
repository. Durable identities are normalized repository-relative paths and
module names rather than process-local graph node IDs.

The index retains the Python inventory, file metadata and BLAKE3 hashes,
module/package/test classification, located parsed imports, resolution
candidates and matches, unresolved states, forward dependencies, reverse
dependents, edge provenance, candidate-to-importer relationships, source
roots, summary counters, compatibility fingerprints, and a Git baseline.
Reverse and candidate relationships are stored as individual pairs, so a
one-edge update does not rewrite a repository-sized adjacency value.

For a Git repository whose selected root is its top level, repeated commands
compare the saved baseline with the current `HEAD`, staged and unstaged state,
untracked files, and ignored Python files that ordinary Urmare discovery would
index. This covers commits after the baseline, branch switches, older
checkouts, rebases, restored dirty files, and removed untracked files. A
no-change run performs no Python parses, resolutions, graph mutations, or
index writes. When Git reports no candidate paths it also performs no Python
reads or hashes. The Git query excludes built-in discovery boundaries such as
`.venv`, `.tox`, and tool-cache directories before their ignored Python paths
are returned. Configured exclusion globs are applied to the returned paths.
Indexed ignored Python paths that remain eligible are conservatively read and
hashed because Git cannot otherwise report their content changes. A changed
file is parsed only when its content hash changed. If its parsed imports are
unchanged, its existing edges and provenance remain intact.

Module additions, removals, renames, and safe remaps use an inverted
candidate index. Urmare re-resolves only imports that considered the changed
module identities; unrelated importers remain untouched. This preserves the
case where an unresolved/external-looking import becomes repository-local, or
the reverse, without invalidating the complete module universe. Moves are
planned as removals plus additions, independent of path ordering. Changes
that alter inferred source roots, including a top-level `src/` convention
appearing or disappearing, conservatively trigger a full rebuild.

Git index states that can hide content from ordinary status, including
`assume-unchanged`, `skip-worktree`, tracked submodules, and nested untracked
repositories, disable the bounded path and force complete discovery.

Impact reads persistent reverse relationships along the reachable frontier;
`why` reads forward records along candidate shortest paths. Their work scales
with the requested result. Complete operations such as unscoped `graph
--debug` and unresolved-import listing scan the corresponding complete index
records because their output is complete. Git deleted and previous-rename
identities remain query overlays and are never committed as current-tree
records.

The storage engine is redb 4.2, a maintained pure-Rust embedded database with
ACID transactions and no production transitive or native/system dependency.
Its Rust 1.90 minimum is below Urmare's Rust 1.95 floor. This keeps the binary
portable across the supported Windows, macOS, and Linux targets without a
runtime database installation. A transaction commits one complete index
generation; readers do not observe partial updates. Concurrent writers are
serialized by the database lock. If another process holds that lock, Urmare
builds a correct uncached in-memory view instead of waiting or returning stale
results.

Missing and incompatible indexes rebuild automatically. Truncated or corrupt
files are replaced, and an interrupted uncommitted transaction leaves the
preceding committed generation intact. Any storage or query failure falls
back to correct uncached analysis. Parser, resolver, index-schema, and relevant
`[tool.urmare]` fingerprint changes invalidate incompatible state.
Configuration changes rebuild the current-tree index under the current valid
configuration while preserving the conservative Git-aware full-validation
contract described above.

Non-Git repositories currently use full discovery and construction on every
invocation because Urmare has no portable bounded-delta proof for them. This
is a correctness fallback, not incremental behavior. The index requires no
daemon, watcher, platform journal, or remote service.

## JSON output

Every current command accepts `--json` for CI systems, scripts, and coding
agents. JSON is written to stdout; failures leave stdout empty and write an
actionable diagnostic to stderr. Impact JSON always contains the complete
result and does not accept the human-only `--all` option.

Graph schema version 1 contains deterministic repository totals and every
unresolved static import:

```json
{
  "schema_version": 1,
  "python_files": 42,
  "modules": 42,
  "import_edges": 128,
  "tests": 9,
  "unresolved_imports": 1,
  "unresolved_import_details": [
    {
      "importer": "src/api/app.py",
      "line": 4,
      "column": 8,
      "import": {
        "kind": "import",
        "module": "fastapi"
      }
    }
  ]
}
```

Passing `graph --debug --json` adds an `inspection` object containing `focus`,
`source_roots`, `modules`, `edges`, and `resolution_traces`. Every edge records
its dependent, dependency, and all located static imports that produced that
unique edge. Every trace contains a deterministic `status`, candidate module
names, and matched local modules with canonical paths. The optional additive
object is omitted by plain `graph --json`.

Impact schema version 1 has this shape:

```json
{
  "schema_version": 1,
  "changed": ["src/payments/models.py", "src/payments/stripe.py"],
  "directly_affected": ["src/payments/service.py"],
  "transitively_affected": ["src/api/checkout.py"],
  "affected_tests": ["tests/api/test_checkout.py"],
  "attributions": [
    {
      "affected": "tests/api/test_checkout.py",
      "caused_by": ["src/payments/models.py", "src/payments/stripe.py"]
    }
  ]
}
```

Test-selection schema version 1 contains `schema_version`, `changed`,
`affected_tests`, and test-only `attributions`. Every field is present even when
its array is empty. Arrays and attribution entries are deterministic, and paths
always use canonical repository-relative `/` notation. Rename analysis lists
both old and new identities in `changed`. Explicit multi-file analysis uses the
same deterministic union and attribution schema.

When the root `pyproject.toml` changed, impact and test-selection outputs add
this optional v1 field:

```json
{
  "full_validation": {
    "required": true,
    "reason": "configuration_changed",
    "configuration_paths": ["pyproject.toml"]
  }
}
```

In that state, `affected_tests` contains every currently eligible discovered
test, `directly_affected` and `transitively_affected` are empty because module
impact is deliberately unavailable, and `attributions` is empty rather than
inventing import-graph attribution from configuration. The `changed` array
continues to contain Python path identities only and can therefore be empty for
a configuration-only change. Consumers must interpret the presence of
`full_validation` before interpreting the selective arrays. The field is an
additive schema-version-1 extension; it is omitted from ordinary results.

Why schema version 1 preserves both canonical endpoints and the ordered
explanation path. Its additive `steps` array records exact import evidence for
each adjacent path pair:

```json
{
  "schema_version": 1,
  "changed": "src/payments/stripe.py",
  "affected": "tests/api/test_checkout.py",
  "path": [
    "tests/api/test_checkout.py",
    "src/api/checkout.py",
    "src/payments/service.py",
    "src/payments/stripe.py"
  ],
  "steps": [
    {
      "dependent": "tests/api/test_checkout.py",
      "dependency": "src/api/checkout.py",
      "imports": [
        {
          "line": 1,
          "column": 17,
          "import": {
            "kind": "from",
            "module": "api",
            "name": "checkout",
            "level": 0
          }
        }
      ]
    },
    {
      "dependent": "src/api/checkout.py",
      "dependency": "src/payments/service.py",
      "imports": [
        {
          "line": 1,
          "column": 30,
          "import": {
            "kind": "from",
            "module": "payments.service",
            "name": "create_payment",
            "level": 0
          }
        }
      ]
    },
    {
      "dependent": "src/payments/service.py",
      "dependency": "src/payments/stripe.py",
      "imports": [
        {
          "line": 1,
          "column": 15,
          "import": {
            "kind": "from",
            "module": null,
            "name": "stripe",
            "level": 1
          }
        }
      ]
    }
  ]
}
```

Incompatible schema changes require a new `schema_version`. Additive fields may
be introduced within a version, so consumers should ignore unknown fields.

## Exit codes

Urmare uses a small stable exit-code contract suitable for scripts and coding
agents:

| Code | Meaning |
| ---: | --- |
| `0` | Analysis completed successfully, including an empty impact result. |
| `1` | Urmare encountered an unexpected internal, serialization, or output failure. |
| `2` | CLI arguments or `[tool.urmare]` configuration are invalid. |
| `3` | The requested repository, Git state, Python source, or dependency path could not be analyzed. |

Blast-radius size never changes the exit code. With `--json`, successful output
is JSON on stdout. Every failure leaves stdout empty, writes one actionable
diagnostic to stderr, and returns the appropriate non-zero code.

Graph edges use this orientation:

```text
A -> B
```

This means “A depends on B.” If B changes, reverse traversal finds A. The `why`
output reads naturally from the affected dependent toward the changed
dependency:

```text
tests/api/test_checkout.py
  -> src/api/checkout.py
     via tests/api/test_checkout.py:1:17  from api import checkout
  -> src/payments/service.py
     via src/api/checkout.py:1:30  from payments.service import create_payment
  -> src/payments/stripe.py
     via src/payments/service.py:1:15  from . import stripe
```

## Architecture

The workspace keeps product responsibilities separate:

- `urmare-graph`: generic compact node IDs, forward/reverse adjacency, reverse
  closure, and deterministic shortest paths
- `urmare-python`: Python file discovery with portable exclusions, module
  mapping, AST import extraction, relative import handling, traceable local
  resolution, and convention/configuration-based test discovery
- `urmare-core`: typed repository configuration, transactional persistent
  repository indexing, bounded Git delta updates, candidate-based invalidation,
  query-facing graph views, resolved-edge provenance, impact orchestration,
  and structured errors, including portable Git command orchestration
- `urmare-cli`: command parsing and human-readable presentation

Repository-relative native `PathBuf` values are canonical inside analysis
results. Absolute paths are confined to filesystem boundaries. CLI paths are
rendered with `/` separators so logically equivalent output is stable across
macOS, Linux, and Windows.

## Python syntax compatibility and parser choice

The source-syntax target is Python 3.9 through Python 3.14. Urmare uses the
pure-Rust `ruff_python_parser` and `ruff_python_ast` crates, currently pinned to
`0.0.7`. Ruff's parser supports Python 3.14 grammar, including template strings,
without consulting the locally installed Python version or binding Urmare to a
CPython ABI. Urmare has parser tests covering syntax introduced across the
target range.

The Ruff parser crates describe themselves as internal components and their API
is pre-1.0. Urmare therefore pins their versions and should review parser
upgrades deliberately. Urmare parses the current grammar; it does not yet infer
a project's minimum Python version or reject newer syntax based on project
metadata.

## Module-resolution assumptions

Module resolution remains centralized and follows this policy:

- configured `tool.urmare.source-roots` are used when present
- configured `tool.urmare.test-roots` classify every Python file below them as
  a test while preserving filename-based pytest discovery elsewhere
- configured `tool.urmare.exclude` globs remove matching paths before analysis
- without configuration, a top-level `src/` directory is the production source
  root; otherwise the repository root is the source root
- tests and Python files outside every selected source root remain rooted at the
  repository root
- the most specific root wins when configured roots overlap
- `__init__.py` maps to its containing package
- namespace-style directory paths can map to dotted modules without requiring
  every parent to contain `__init__.py`
- local package prefixes are included when Python import execution loads them

Explicit roots supplement rather than burden the zero-configuration path:
ordinary flat and `src/` repositories do not need a `pyproject.toml` entry.

## Current limitations

- non-Git repositories conservatively fall back to complete discovery and
  construction on every invocation
- cold, incompatible, configuration-invalidated, corrupt, and source-root
  remap cases require complete discovery and index reconstruction
- Git delta detection invokes Git for ignored Python paths so files included
  by ordinary discovery are not silently omitted; built-in environment/cache
  directories are excluded in the Git pathspec, while custom configuration
  exclusions are filtered after Git returns matching paths
- Git safety validation inspects tracked Python index flags; the delta phase
  can therefore scale with the tracked Python inventory even when parsing,
  resolution, graph mutation, persistence, and narrow queries remain bounded
- complete graph/debug and unresolved-import output scans complete relevant
  index tables; broad impact remains proportional to its result
- exclusion patterns form one additive set; ordered `!` re-inclusion is not
  supported
- no sophisticated monorepo source-root inference
- static `import` and `from ... import ...` relationships only
- no dynamic import, re-export, symbol, call, type, fixture, or framework graph
- test selection is file-level; it does not select individual test functions
- unresolved diagnostics mean “no repository-local module matched”; installed
  environments and package indexes are not inspected, and external packages
  are not indexed
- no test execution, CI execution, MCP, or hosted integration

When a static import could name either a package export or a local submodule,
Urmare includes the certain local package/module loads. This intentionally
favors impact recall over minimizing false positives.

## Development

Run the required checks before submitting changes:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo +1.95.0 check --workspace --all-targets --locked
```

Purpose-built fixture repositories live under `fixtures/python-projects/`.

## Performance benchmarks

Run the deterministic synthetic benchmark with:

```bash
cargo bench -p urmare-core --bench synthetic --locked
```

It generates temporary 1,000- and 10,000-file Git repositories and separately
measures cold creation, clean reuse, content/import edits, module add/delete,
rename, configuration rebuild, and narrow/broad impact. Every case validates
bounded work counters and compares observable results with a fresh uncached
analysis. Set `URMARE_BENCH_50000=1` to opt into the 50,000-file case. No
generated large fixture is committed. See
[docs/performance.md](docs/performance.md) for the methodology and current
reference observation.

The separate real-project suite targets pinned releases of Flask, FastAPI,
Django, pandas, and Apache Airflow. It does not publish or update benchmark
claims automatically. Build one optimized CLI and its release-mode profiling
helper from the same clean commit:

```bash
cargo build --release --locked -p urmare-cli --bin urmare
cargo build --release --locked -p urmare-core --example profile_repository
```

Then prepare the pinned corpus while network access is available, inspect the
fully resolved run without measuring, execute the default 15 paired samples,
and generate a proposed summary from preserved raw JSON Lines:

```bash
python3 benchmarks/real_projects/benchmark.py prepare \
  --work-dir target/real-project-benchmark
python3 benchmarks/real_projects/benchmark.py dry-run \
  --work-dir target/real-project-benchmark \
  --binary target/release/urmare \
  --profile-helper target/release/examples/profile_repository
python3 benchmarks/real_projects/benchmark.py run \
  --work-dir target/real-project-benchmark \
  --binary target/release/urmare \
  --profile-helper target/release/examples/profile_repository \
  --samples 15 \
  --output target/real-project-benchmark/raw/official.jsonl
python3 benchmarks/real_projects/benchmark.py summarize \
  --input target/real-project-benchmark/raw/official.jsonl \
  --output target/real-project-benchmark/summary/official.md
```

Only `prepare` needs network access. Exercise the same lifecycle offline on a
small local fixture with:

```bash
python3 benchmarks/real_projects/benchmark.py smoke \
  --work-dir target/real-project-smoke \
  --binary target/release/urmare \
  --profile-helper target/release/examples/profile_repository \
  --output target/real-project-smoke/raw.jsonl
```

See [docs/performance.md](docs/performance.md) for corpus selection, cache and
checkout isolation, warm-up policy, raw schema, correctness checks, timing
boundaries, statistics, resume safety, and limitations. Synthetic scaling and
public-repository observations remain distinct benchmark families.

## Next implementation slice

The next performance slice should profile the remaining Git delta-detection
cost and evaluate bounded filesystem validation for additional repository
shapes. A non-Git fast path still needs a portable way to prove a complete
delta; it must not be implemented by silently weakening discovery semantics.

## License

Urmare is available under the MIT License.
