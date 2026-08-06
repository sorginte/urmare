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
- detect committed, staged, unstaged, and untracked Python changes relative to
  a Git merge base
- union impact across multiple Git-changed files with per-result attribution
- preserve conservative impact for deleted and renamed modules
- emit stable, versioned JSON for every current command
- persist versioned parsed imports, module identities, and resolved local edges
  for incremental repository indexing
- expose measured discovery, parsing, and graph-construction phases for
  reproducible performance work
- discover `test_*.py` and `*_test.py` files plus configured test trees
- select affected test files
- return one deterministic shortest dependency path with import evidence for
  every hop
- emit actionable errors for invalid input, configuration, and Python syntax

Urmare currently analyzes static imports only. It does not claim symbol-level,
call-graph, runtime, or framework-specific semantic understanding.

## Build

Urmare is a Rust workspace and currently builds with stable Rust:

```bash
git clone https://github.com/sorginte/urmare.git
cd urmare
cargo build --release
```

The CLI binary is written to `target/release/urmare` (`urmare.exe` on Windows).
Prebuilt distribution is planned, but release packaging is intentionally outside
this MVP.

For development, commands can be run without installing the binary:

```bash
cargo run -p urmare-cli -- graph
```

## Usage

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
urmare impact --git-diff main
urmare impact --git-diff main --json
urmare tests --affected src/example/service.py
urmare tests --affected src/example/service.py src/example/models.py
urmare tests --affected --git-diff main
urmare tests --affected --git-diff main --json
urmare why src/example/service.py tests/test_api.py
urmare why src/example/service.py tests/test_api.py --json
```

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

```text
Graph inspection

Module mappings (1)
  src/payments/service.py -> payments.service [source; 2 dependencies; 1 dependents]

Resolved import edges (3)
  src/payments/service.py -> src/payments/stripe.py
    via src/payments/service.py:1:15  from . import stripe

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
files and `--git-diff` are mutually exclusive.

Git-aware analysis compares the working tree with the merge base of the
provided revision and `HEAD`. It includes committed branch changes, staged and
unstaged changes, and untracked files that are not ignored by Git:

```bash
urmare impact --git-diff origin/main
urmare tests --affected --git-diff origin/main
```

Renames seed impact from both the old and new module identities. Deleted module
paths receive virtual graph nodes, allowing surviving imports and downstream
tests to remain connected without checking out the base revision. Deleted test
files themselves are not emitted as runnable affected tests.

Git-aware analysis requires the `git` executable, and the selected root
(explicit or `.` by default) must currently be the Git repository's top-level
directory.

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

## Incremental cache

Normal analysis automatically stores parsed static imports and a resolved
local graph index in Urmare's platform-standard per-user cache directory.
Cache entries are isolated by the canonical repository root, so no cache files
are written into the analyzed repository.

Unchanged size and modification metadata provide the fast path. When metadata
changes, Urmare reads the source and compares a BLAKE3 content hash before
deciding whether AST parsing is necessary. Changed content is reparsed. The
parsed-import cache header includes its schema version, the Python
parser/import-extraction version, and normalized source-root, test-root, and
exclude configuration; incompatible data is ignored and rebuilt. Located
imports and unresolved-resolution details are versioned with their respective
cache documents.

The graph index separately stores each path's module identity and the complete
candidate/match result for every located import. This retains resolved-edge
provenance, resolution traces, and unresolved-import details across warm runs.
A file can reuse its resolved edges only when its parsed imports were also
reused and the complete `(path, module)` set is unchanged. Adding, deleting,
renaming, or remapping any module invalidates all resolved edges. This
conservative rule is important for impact recall: an unchanged `import
candidate` may become local when `candidate.py` is added, or become external
when that file is removed. Duplicate-module checks still run across every
current path on every build.

Cache hashing uses BLAKE3's portable pure-Rust implementation, and cache
locations come from the operating system's standard per-user directories. This
keeps cache storage cross-platform without adding a runtime system dependency.

Cache writes are best-effort and atomic. A missing, read-only, interrupted, or
corrupt cache never prevents analysis and cannot replace current file
discovery. Every command still allocates an immutable in-memory graph, but it
can populate that graph from cached identities and resolved edge lists instead
of remapping and re-resolving every unchanged file. Git deletion and rename
analysis retains its virtual old-path identities and applies the same
module-set safety rule.

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
    }
  ]
}
```

Incompatible schema changes require a new `schema_version`. Additive fields may
be introduced within a version, so consumers should ignore unknown fields.

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
- `urmare-core`: typed repository configuration, versioned parsed-import and
  graph-index caches, repository indexing, resolved-edge provenance, graph
  inspection domain results, impact orchestration, and structured errors,
  including portable Git command orchestration
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

- discovery and in-memory graph allocation still run for every command; cached
  identities and resolved edges avoid repeated mapping/resolution work
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
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Purpose-built fixture repositories live under `fixtures/python-projects/`.

## Performance benchmarks

Run the deterministic warm-performance benchmark with:

```bash
cargo bench -p urmare-core --bench synthetic
```

It generates temporary 1,000- and 10,000-file Python repositories, measures the
real discovery, parsing, graph-construction, complete-build, and impact paths;
validates exact parsed-import and graph-index reuse counts; and checks the
expected graph and affected-test counts. No generated large fixture is
committed. See [docs/performance.md](docs/performance.md) for the workload,
phase definitions, reference observation, and standalone generator.

## Next implementation slice

The next recommended slice is true incremental indexing: avoid full discovery
and in-memory graph reconstruction when repository state proves that only a
small set of files changed. The existing versioned parsed-import and resolution
caches provide the safe persisted inputs; the next design must preserve impact
recall across additions, deletions, renames, configuration changes, and module
universe changes.

## License

Urmare is available under the MIT License.
