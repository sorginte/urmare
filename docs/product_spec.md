# Urmare Product Specification

## Organization

**Sorginte**

*Sorginte* is Romanian for **source, origin, or beginning**. Sorginte is an open-source developer-tools organization.

## Product

**Urmare**

*Urmare* is Romanian for **what follows, consequence, result, or effect**.

The name expresses the core product question:

> What follows from this code change?

The intended public repository is `github.com/sorginte/urmare`.

---

# Product Vision

Urmare is a high-performance repository intelligence engine for Python.

Its primary purpose is to answer:

> What is the blast radius of this change?

Urmare builds a dependency graph of a Python repository and combines it with source changes to determine:

- what modules are affected
- what tests are affected
- what packages are affected
- why they are affected
- what validation should run

The long-term ambition is to become the repository intelligence layer used by developers, CI systems, and coding agents.

---

# Problem

Python developers routinely make small changes but run disproportionately large amounts of validation.

Examples:

```text
change 1 file
run 7,000 tests

change 3 modules
rebuild 14 services

modify internal function
run the entire monorepo CI pipeline
```

Teams often lack a reliable answer to:

```text
What depends on this?

Which tests are actually relevant?

Which applications could this change affect?

Is this change low-risk or high-risk?

Why does this test need to run?

What should an AI coding agent validate after editing this code?
```

Existing dependency tooling is often:

- too low-level
- focused only on imports
- embedded in heavyweight build systems
- difficult to adopt incrementally
- not optimized for ordinary Python repositories

Urmare should provide this intelligence without requiring teams to replace their existing tools.

---

# Product Positioning

Urmare is not a Python build system.

Urmare is not a package manager.

Urmare is not a test framework.

Urmare is not a CI provider.

Urmare is:

> The high-performance impact-analysis engine for Python repositories.

Alternative concise positioning:

> Understand the blast radius of every Python change.

---

# Open-Source Mission

Urmare is intended to be a meaningful contribution to the Python ecosystem.

The project should:

- remain fully useful as open-source software
- favor standards and existing Python conventions
- avoid unnecessary lock-in
- expose deterministic and explainable analysis
- be approachable to external contributors
- integrate with existing tools instead of replacing them
- publish reproducible benchmarks rather than invented performance claims

---

# Correctness Principle

Urmare optimizes primarily for **impact recall**.

For impact analysis, a false negative is more dangerous than a false positive:

- a false positive may cause an unnecessary test or validation step to run
- a false negative may allow affected code to go unvalidated

Therefore Urmare should prefer conservative over-selection whenever analysis is uncertain.

This principle should guide:

- import resolution
- affected-module calculation
- affected-test selection
- future confidence levels
- future CI planning

When uncertainty can be identified, Urmare should expose it instead of presenting uncertain analysis as certain.

---

# Python Syntax Compatibility

The initial source-syntax compatibility target is:

```text
Python 3.9 through Python 3.14
```

Urmare analyzes Python source code and should not require those Python interpreters to be installed.

The parser and analysis architecture should support valid syntax used by repositories targeting that range. Parser selection should explicitly consider this compatibility requirement.

This is a source-analysis compatibility target, not a promise that Urmare executes or emulates Python runtime semantics for every supported version.

---

# Canonical Path Model

Repository-relative normalized paths are the canonical representation of files in user-facing and machine-readable Urmare results.

Examples:

```text
src/payments/stripe.py
tests/api/test_checkout.py
```

Absolute OS-specific paths should not become durable public identifiers.

The implementation should:

- discover files using native filesystem path handling
- normalize them relative to the detected repository root
- use repository-relative identity for graph/domain models where practical
- serialize paths in a stable cross-platform form
- avoid leaking Windows drive letters or machine-specific absolute prefixes into JSON results

This is important for reproducible CI output, caching, agent integrations, and cross-platform behavior.

---

# Target Users

## Individual Python developers

Needs:

- understand unfamiliar repositories
- determine what code depends on a file
- know what tests to run
- avoid breaking unrelated functionality

---

## Large Python teams

Needs:

- reduce CI time
- understand monorepo dependency relationships
- assess risky changes
- troubleshoot cascading failures

---

## Platform engineering teams

Needs:

- produce selective CI execution plans
- reduce compute usage
- enforce validation policies
- expose repository dependency intelligence centrally

---

## Coding agents

Needs:

- understand repository structure
- determine affected code
- select validation
- explain dependency paths
- minimize unnecessary tool calls and token usage

---

# Core User Stories

## Story 1: File impact

A developer runs:

```bash
urmare impact src/payments/service.py
```

Urmare responds:

```text
Impact analysis

Changed (1)
  src/payments/service.py

Summary
  Directly affected modules       3
  Transitively affected modules   9
  Affected tests                 38

Directly affected modules (3)
  src/api/checkout.py
  src/payments/commands.py
  src/payments/refunds.py

Transitively affected modules (9)
  ...

Affected tests (38)
  tests/api/test_checkout.py
  tests/payments/test_service.py
  ...
```

Human-readable lists may be truncated for large results and must tell the user
that `--all` displays every entry. JSON output is always complete.

---

## Story 2: Affected tests

A developer runs:

```bash
urmare tests --affected src/payments/stripe.py
```

Urmare outputs only test files affected by the changes.

Eventually it may invoke pytest, but selecting affected tests is the primary capability.

---

## Story 3: Explain dependency

A developer asks:

```bash
urmare why src/payments/stripe.py tests/api/test_checkout.py
urmare why src/payments/stripe.py tests/api/test_checkout.py --changed
urmare why src/payments/stripe.py tests/api/test_checkout.py --git-diff origin/main
```

Urmare responds:

```text
tests/api/test_checkout.py
  -> src/api/checkout.py
     via tests/api/test_checkout.py:1:17  from api import checkout
  -> src/payments/service.py
     via src/api/checkout.py:1:30  from payments.service import create_payment
  -> src/payments/stripe.py
     via src/payments/service.py:1:15  from . import stripe
```

---

## Future Story: CI integration

A CI workflow runs:

```bash
urmare impact --git-diff origin/main --json
```

and uses the structured result to determine which validations should execute.

---

# MVP

The MVP proves one hypothesis:

> Static Python import analysis can reliably identify a useful blast radius and reduce unnecessary test execution with near-zero configuration.

The MVP consists of five capabilities.

---

## 1. Repository discovery

Urmare identifies:

- repository root
- Python files
- test files
- project roots

Support common layouts:

```text
project/
  package/
```

and:

```text
project/
  src/
    package/
```

---

## 2. Python import graph

Parse Python source and extract imports.

Support:

```python
import foo
import foo.bar
from foo import bar
from foo.bar import baz
from . import foo
from ..foo import bar
```

Map imports back to local repository modules where possible.

External dependencies should not automatically become internal graph nodes unless needed for diagnostics.

---

## 3. Reverse dependency traversal

Given one or more changed files/modules, calculate:

```text
direct dependents
transitive dependents
```

This is the initial blast radius.

---

## 4. Test impact

Discover pytest-compatible test files.

Determine which test files fall inside the reverse dependency closure of changed code.

Initial granularity:

```text
test file
```

Not:

```text
individual test function
```

---

## 5. Explanation paths

Urmare must explain why an affected module/test is connected to a changed module.

The user should be able to inspect at least one valid dependency path.

---

# MVP Commands

The current CLI grammar is:

```text
urmare [--root PATH] graph [--json|--all] [--debug [--focus FILE]]
urmare [--root PATH] impact <FILE...|--changed|--git-diff BASE> [--json|--all]
urmare [--root PATH] tests --affected <FILE...|--changed|--git-diff BASE> [--json]
urmare [--root PATH] why CHANGED_FILE AFFECTED_FILE [--changed|--git-diff BASE] [--json]
```

Each command's alternative change sources are mutually exclusive. An explicit
`--root` is authoritative. Every Git-aware form discovers the containing Git
top level when `--root` is omitted.

## `urmare graph`

Build and inspect repository graph.

Support stable machine-readable output:

```bash
urmare graph --json
```

Human unresolved-import diagnostics are bounded by default:

```bash
urmare graph --all
```

Detailed graph inspection and local module-resolution tracing are opt-in:

```bash
urmare graph --debug
urmare graph --debug --focus src/payments/service.py
urmare graph --debug --json
```

`--debug` reports inferred source roots, canonical path-to-module mappings,
resolved edges with every located import that created them, and deterministic
resolution traces. A trace contains all dotted module candidates considered,
all repository-local matches, and one of these statuses:

```text
resolved
unresolved
invalid_relative_import
```

`unresolved` means no candidate matched a repository-local module. An invalid
relative import is reported separately when it ascends above the importer
package. `--focus <file>` requires `--debug`; it scopes module mappings and
originating resolution traces to that file while retaining incident incoming
and outgoing edges. Human debug sections are bounded unless `--all` is passed.
Debug JSON is complete.

Potential output:

```text
Repository graph

Python files:        1,284
Modules:             1,231
Import edges:        7,482
Tests:                 314
Unresolved imports:     17

Unresolved import details (17)
  No repository-local module matched; external packages are not resolved.
  src/api/app.py:14:8  import fastapi
  ...

Indexed in 84 ms
```

---

## `urmare impact`

Example:

```bash
urmare impact src/payments/stripe.py
urmare impact src/payments/stripe.py src/payments/models.py
urmare impact --changed
urmare impact --changed --json
urmare impact --git-diff main
urmare impact --git-diff main --json
```

Output:

```text
Impact analysis

Changed (1)
  src/payments/stripe.py

Summary
  Directly affected modules       4
  Transitively affected modules  21
  Affected tests                 13

Directly affected modules (4)
  src/payments/service.py
  ...

Transitively affected modules (21)
  src/api/checkout.py
  ...

Affected tests (13)
  tests/api/test_checkout.py
  ...
```

Test files are listed in the affected-tests section rather than duplicated in
the module sections. Human output shows a bounded number of entries per section
unless `--all` is provided. `--json` remains complete and machine-readable.

Explicit impact accepts one or more changed files. Urmare normalizes duplicate
or equivalent paths, unions their reverse dependency closures, and retains all
changed-file attribution for each result. Explicit paths, `--changed`, and
`--git-diff` are mutually exclusive.

`--changed` compares the working tree to `HEAD` and includes staged, unstaged,
and untracked non-ignored Python paths, including added, deleted, and renamed
files. `--git-diff <base>` additionally includes committed branch changes since
the merge base of `<base>` and `HEAD`. When no explicit `--root` is supplied,
Git-aware impact discovers the containing Git repository top level so it can be
invoked from a subdirectory.

---

## `urmare tests --affected`

Example:

```bash
urmare tests --affected src/payments/stripe.py
urmare tests --affected src/payments/stripe.py src/payments/models.py
urmare tests --affected --changed
urmare tests --affected --changed --json
urmare tests --affected --git-diff main
urmare tests --affected --git-diff main --json
```

Output:

```text
tests/payments/test_service.py
tests/payments/test_stripe.py
tests/api/test_checkout.py
```

Support `--json`. Test selection accepts exactly one change source: one or more
explicit files, `--changed`, or `--git-diff <base>`. The Git-aware forms use
the same working-tree, merge-base, repository-root discovery, deletion, and
rename semantics as `urmare impact`.

---

## `urmare why`

Example:

```bash
urmare why src/payments/stripe.py tests/api/test_checkout.py
urmare why src/payments/stripe.py tests/api/test_checkout.py --json
urmare why src/payments/stripe.py tests/api/test_checkout.py --changed
urmare why src/payments/stripe.py tests/api/test_checkout.py --git-diff main
urmare why src/payments/stripe.py tests/api/test_checkout.py --git-diff main --json
```

The non-Git form resolves both paths from the current repository exactly as
before. `--changed` and `--git-diff <base>` are mutually exclusive and require
the changed path to belong to that selected Git change set. Git-aware
explanations use the same current graph and virtual identities as Git-aware
impact, so a deleted path or the previous path of a rename remains explainable
without existing on disk. The affected file must remain currently indexed.
Paths may not escape the repository. Invalid bases, unselected changed paths,
unavailable affected files, and missing dependency paths produce actionable
analysis errors; JSON failures leave stdout empty.

Output:

```text
tests/api/test_checkout.py
  -> src/api/checkout.py
     via tests/api/test_checkout.py:1:17  from api import checkout
  -> src/payments/service.py
     via src/api/checkout.py:1:30  from payments.service import create_payment
  -> src/payments/stripe.py
     via src/payments/service.py:1:15  from . import stripe
```

---

# Blast Radius Definition

For MVP:

A module A depends on module B when A contains a statically resolvable import of B.

An edge is represented as:

```text
A -> B
```

meaning:

```text
A depends on B
```

If B changes, A is directly affected.

If:

```text
A -> B
B -> C
```

and C changes, both B and A are affected.

The blast radius of changed node C is therefore the reverse transitive dependency closure of C.

For multiple changed nodes, take the union of their reverse dependency closures.

---

# Future Semantic Graph

The graph should evolve beyond module imports.

Future node types:

```text
file
module
package
class
function
method
test
endpoint
service
scheduled job
configuration
```

Future relationships:

```text
IMPORTS
CALLS
INHERITS
IMPLEMENTS
USES_TYPE
TESTS
EXPOSES
CONFIGURES
DEPLOYS
```

---

# Confidence Model

Python code may include dynamic behavior that static analysis cannot resolve reliably.

Eventually graph edges should expose:

```text
CERTAIN
LIKELY
POSSIBLE
```

Examples:

Certain:

```text
static import
direct call
inheritance
explicit type use
```

Likely:

```text
pytest fixture
FastAPI dependency injection
framework registration
```

Possible:

```text
dynamic import
reflection
getattr
plugin discovery
```

The product should communicate uncertainty rather than pretending complete knowledge.

---

# Risk / Blast Radius Score

A deterministic risk model may later combine:

```text
number of dependents
graph depth
package boundaries
service boundaries
public API exposure
signature changes
affected tests
dynamic/uncertain relationships
```

A future specification must define the inputs, weighting, thresholds, and
explanations before any score or classification is exposed. This feature is not
required for the first MVP.

Until that model is formally specified and implemented, Urmare must not emit
risk scores or `LOW`, `MEDIUM`, or `HIGH` risk/blast-radius classifications.
Counts and explainable dependency paths are the deterministic MVP output.

---

# Configuration

Goal:

```text
zero configuration for common repositories
```

Urmare should infer:

- repository root
- source roots
- package structure
- tests

Optional configuration lives in:

```toml
[tool.urmare]
```

The current repository-boundary fields are:

```toml
[tool.urmare]
source-roots = ["src"]
test-roots = ["tests"]
exclude = ["generated/**"]
```

Configuration should supplement inference, not replace it.

`source-roots` controls Python module identity. `test-roots` supplements pytest
filename conventions by classifying every discovered Python file beneath a
configured root as a test. `exclude` contains repository-relative portable
glob patterns applied before parsing and graph construction; `/` is the
configuration separator on every operating system, `*` stays within one path
component, and `**` may cross directory boundaries. A matched directory prunes
its subtree. Exclusions take precedence over configured roots and also apply to
Git-aware change selection.

All configured roots must be non-empty repository-relative directories and may
not escape through `..`. Invalid, absolute, or duplicate roots and patterns are
actionable configuration errors. Built-in ignores for Git metadata, common
virtual environments, and Python tool caches remain active independently of
configuration.

The repository-root `pyproject.toml` is itself an analysis-boundary input. If
Git reports it as added, modified, deleted, or renamed, selective impact is not
safe because source roots, test roots, exclusions, module identities, import
resolution, and indexed tests may all have changed. In this state:

- Git-aware impact reports that full validation is required, leaves selective
  module classifications unavailable, and selects every currently eligible
  discovered test;
- Git-aware affected-test selection returns that same complete current test
  set, including configured test roots and respecting current exclusions;
- human output explicitly reports the fallback, and JSON includes the additive
  `full_validation` object;
- Git-aware `why` fails with a full-validation diagnostic instead of presenting
  a potentially invalid selective explanation.

A configuration-only change must never appear to be an ordinary empty impact
result. Configuration is not modeled as an import-graph node.

---

# JSON Output

Machine-readable output is a first-class requirement.

All MVP commands support `--json`. Successful output includes
`"schema_version": 1`; incompatible schema changes require a new version.
Failures write no partial JSON to stdout and return the normal non-zero CLI
status with an actionable diagnostic on stderr.

Graph output serializes repository totals and the complete structured set of
imports that did not match any repository-local module:

```json
{
  "schema_version": 1,
  "python_files": 1284,
  "modules": 1231,
  "import_edges": 7482,
  "tests": 314,
  "unresolved_imports": 1,
  "unresolved_import_details": [
    {
      "importer": "src/api/app.py",
      "line": 14,
      "column": 8,
      "import": {
        "kind": "import",
        "module": "fastapi"
      }
    }
  ]
}
```

Locations are one-indexed and point to the imported target. For `from`
imports, the nested import object contains `kind`, nullable `module`, `name`,
and relative `level`. Human output may truncate with guidance to use `--all`;
JSON is never truncated.

An unresolved diagnostic means only that no repository-local module matched.
Urmare does not inspect installed environments or package indexes, does not
classify these entries as errors, and does not create external graph nodes.

When `graph --debug --json` is requested, graph schema version 1 adds an
`inspection` object. It contains optional `focus`, `source_roots`, module
mappings with forward/reverse neighbors, unique resolved edges with all source
imports, and complete resolution traces. Plain `graph --json` omits this
optional additive object and retains the concise summary shape.

Example conceptual schema:

```json
{
  "schema_version": 1,
  "changed": [
    "src/payments/stripe.py"
  ],
  "directly_affected": [
    "src/payments/service.py"
  ],
  "transitively_affected": [
    "src/api/checkout.py"
  ],
  "affected_tests": [
    "tests/api/test_checkout.py"
  ]
}
```

When repository-root configuration changed, impact and test-selection schema
version 1 add this optional object:

```json
{
  "full_validation": {
    "required": true,
    "reason": "configuration_changed",
    "configuration_paths": ["pyproject.toml"]
  }
}
```

`affected_tests` then contains every test discovered under the current
configuration. Selective module arrays and `attributions` are empty because
configuration is not an import-graph node; the presence of `full_validation`
distinguishes this state from zero impact. `changed` remains a list of Python
identities and may be empty for a configuration-only change. This field is an
additive version-1 extension and is omitted from ordinary output.

Dependency explanations serialize canonical endpoints and the ordered path
from affected dependent toward changed dependency. They also contain an
additive `steps` array with the exact import evidence for every path hop:

```json
{
  "schema_version": 1,
  "changed": "src/payments/stripe.py",
  "affected": "tests/api/test_checkout.py",
  "path": [
    "tests/api/test_checkout.py",
    "src/api/checkout.py",
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

Schema stability becomes increasingly important once CI/agent integrations exist.

All serialized file paths should use the canonical repository-relative normalized representation rather than machine-specific absolute paths.

## Exit codes

The CLI exit-code contract is:

```text
0  successful analysis, including an empty result
1  unexpected internal, serialization, or output failure
2  invalid CLI usage or tool.urmare configuration
3  requested repository, Git state, Python input, or dependency path could not be analyzed
```

Blast-radius size is output, not command status. JSON failures follow the same
codes, leave stdout empty, and write an actionable diagnostic to stderr.

---

# Performance Goals

These are directional targets, not hard MVP acceptance criteria.

Target warm performance:

```text
1,000 files     < 100 ms
10,000 files    < 500 ms
50,000 files    < 2 s
```

Impact traversal after graph construction should generally be measured in milliseconds.

Initial indexing may take longer. Repeated runs already reuse persistent
parsed-import and graph-resolution caches, subject to conservative
invalidation.

Do not sacrifice correctness for benchmark screenshots.

---

# Incremental Analysis

Urmare currently persists versioned parsed imports, module identities, complete
import-resolution results, and resolved-edge provenance. These caches avoid
repeating safe parsing and resolution work, but they do not yet make discovery
or the in-memory graph incremental: every invocation still discovers the
repository and constructs a complete immutable graph view.

True incremental discovery and in-memory graph updates remain post-MVP work.
Instead of rebuilding the complete current view, that future design may use:

```text
git/status changes
      ↓
changed files only
      ↓
update graph edges
      ↓
recalculate affected subgraph
```

Potential cache inputs:

```text
file path
mtime
size
content hash
parser version
configuration hash
```

---

# CI Vision

Eventually:

```bash
urmare ci --git-diff origin/main
```

could output a validation plan:

```text
Run:
  ruff src/payments
  ty src/payments
  pytest tests/payments tests/api/test_checkout.py

Skip:
  analytics
  notifications
  reporting

Tests avoided:
  8,412
```

Urmare should initially produce the plan rather than attempt to become a CI execution platform.

---

# Coding-Agent Vision

Expose repository intelligence through JSON and eventually MCP.

Potential tools:

```text
get_impact(files)
get_dependencies(entity)
get_dependents(entity)
get_affected_tests(files)
explain_dependency(source, target)
get_validation_plan(files)
```

Example workflow:

```text
agent edits code
    ↓
Urmare impact analysis
    ↓
affected tests/checks
    ↓
agent validates
    ↓
fixes failures
```

This may become one of the project's strongest long-term differentiators.

---

# Distribution and Release Vision

Distribution is a first-class product experience, but release infrastructure is post-MVP.

The guiding principle is:

> One source commit and one Git tag should produce one Urmare version consistently across every supported distribution channel.

The release system should eventually build and publish multiple platform-specific artifacts automatically.

## Canonical release source

GitHub Releases should initially be the canonical source for raw prebuilt binaries.

A release such as:

```text
v0.1.0
```

should eventually correspond to platform-specific archives similar to:

```text
urmare-v0.1.0-aarch64-apple-darwin.tar.gz
urmare-v0.1.0-x86_64-apple-darwin.tar.gz

urmare-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
urmare-v0.1.0-aarch64-unknown-linux-gnu.tar.gz

urmare-v0.1.0-x86_64-unknown-linux-musl.tar.gz
urmare-v0.1.0-aarch64-unknown-linux-musl.tar.gz

urmare-v0.1.0-x86_64-pc-windows-msvc.zip
```

The exact naming may evolve.

## Initial platform targets

Priority targets:

```text
macOS ARM64
macOS x86-64
Linux glibc x86-64
Linux glibc ARM64
Windows x86-64
```

Early follow-up targets:

```text
Linux musl x86-64
Linux musl ARM64
```

Potential later target:

```text
Windows ARM64
```

Do not create per-distribution builds for Ubuntu, Debian, Fedora, RHEL, and similar distributions. Linux compatibility should be handled through appropriate glibc/manylinux and musl targets instead.

## PyPI and uv

Although Urmare is written in Rust, Python developers are the primary audience.

The preferred eventual user experience is:

```bash
uv tool install urmare
```

or:

```bash
uvx urmare impact --git-diff main
```

PyPI should therefore be a first-class distribution channel.

Where possible, Python packaging should ship the standalone Urmare binary in platform-specific wheels rather than requiring compilation on the user's machine.

The packaging design should avoid coupling Urmare unnecessarily to individual CPython ABIs or requiring a separate artifact matrix for every Python minor version.

Publishing should eventually use trusted/OIDC-based publishing rather than long-lived repository secrets where supported.

## crates.io

Urmare should also be publishable through crates.io:

```bash
cargo install urmare
```

This is a secondary installation path for Rust users and contributors, not the primary installation path for Python developers.

## Homebrew

A Sorginte Homebrew tap is an expected post-launch channel:

```bash
brew install sorginte/tap/urmare
```

A future repository may be:

```text
github.com/sorginte/homebrew-tap
```

## Standalone installers

Future releases should support a simple shell installer on Unix-like systems and PowerShell installer on Windows.

These installers should:

- detect operating system
- detect CPU architecture
- select the correct prebuilt artifact
- verify integrity
- install the binary

They should consume the canonical release artifacts rather than compile Urmare locally.

## Sorginte website

The Sorginte website is primarily:

- the organization presentation site
- Urmare's product landing page
- documentation
- installation instructions
- links to releases

Initially, binaries do not need to be hosted directly by Sorginte infrastructure.

The website can link to or redirect to GitHub Release artifacts and PyPI. A future Sorginte-controlled release mirror or CDN may be added if adoption warrants it.

## Release integrity

Every public release should eventually include:

- cryptographic checksums
- reproducible release metadata where practical
- provenance or attestations from the release workflow

Later stages may add:

- SBOMs
- macOS signing and notarization
- Windows code signing
- additional package managers

## Architectural implication

Because Urmare will be distributed as prebuilt binaries across operating systems and CPU architectures:

- keep the core binary portable
- minimize unnecessary native dependencies
- avoid assumptions tied to a specific Linux distribution
- keep path handling cross-platform
- prefer dependencies that cross-compile cleanly
- avoid requiring Rust on end-user machines
- avoid unnecessary CPython ABI coupling

These constraints should influence implementation decisions now even though the release pipeline itself is not part of the MVP.

---

# Sorginte Product Family

Urmare is the first Sorginte product.

A possible future product is **Miez** — Romanian for *core* or *essence* — focused on producing minimal application/container artifacts containing only what an application actually needs.

Miez is a separate project and explicitly outside Urmare's scope.

---

# Non-Goals for MVP

Do not implement:

- package installation
- dependency resolution from package indexes
- test framework replacement
- distributed execution
- remote cache
- full Python call graph
- runtime tracing
- GitHub application
- web dashboard
- LLM functionality
- MCP server
- service discovery
- Docker analysis
- framework-specific semantics
- PyPI packaging
- GitHub release automation
- Homebrew publishing
- installer generation
- signing or notarization

Unless an MVP requirement strictly depends on one of these, defer it.

---

# Success Criteria

The MVP is successful if it can demonstrate:

```text
Repository:
  thousands of Python files
  thousands of tests

Change:
  small number of Python files

Urmare:
  identifies affected dependency closure
  selects relevant test files
  explains why those tests are affected

Result:
  materially fewer tests need to run
```

The ideal demo:

```text
Repository: 18,492 Python files
Tests:       8,731

Changed:
  src/billing/pricing.py

Affected:
  17 modules
  46 tests

46 / 8,731 tests selected
99.5% of tests avoided
```

while maintaining a trustworthy dependency explanation.

---

# Guiding Product Principle

Whenever there is tension between adding breadth and making impact analysis excellent, choose impact analysis.

The first milestone is not:

> Build a complete Python repository graph.

It is:

> Make `urmare impact` surprisingly useful.

Sorginte's broader open-source philosophy:

> Build useful things. Leave them open.
