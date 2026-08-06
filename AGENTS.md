# AGENTS.md

## Project Identity

**Organization:** Sorginte  
**Product:** Urmare

*Sorginte* is Romanian for **source, origin, or beginning**. Sorginte is an open-source developer-tools organization and a home for tools that improve how developers understand, build, and operate software.

*Urmare* is Romanian for **what follows, consequence, result, or effect**. Urmare is Sorginte's first open-source product.

The product's core question is:

> Given a Python code change, what follows from it?

The intended public repository is `github.com/sorginte/urmare`, and the CLI command is `urmare`.

## Project Overview

This repository contains a high-performance Python repository intelligence and impact-analysis engine.

The product answers one core question:

> If this Python code changes, what does it affect?

The engine builds a semantic graph of a Python repository and uses that graph to calculate blast radius, identify affected modules and tests, explain dependency paths, and eventually provide execution plans to CI systems and coding agents.

The initial product should remain intentionally narrow.

## Open-Source Philosophy

Urmare is intended to be a serious contribution to the Python open-source community.

Optimize for:

- usefulness to Python developers
- transparent and explainable engineering
- approachable contribution workflows
- clear documentation
- deterministic behavior
- strong performance
- composability with existing Python tools
- no dependency on a hosted Sorginte service for core functionality

Core engineering decisions should be explainable publicly. Avoid unnecessary proprietary dependencies.


We are not building:

- a package manager
- a Python build system
- a test framework
- a linter
- a formatter
- a type checker
- a CI platform

Instead, we build the intelligence layer that tells those tools what needs to run.

The design philosophy should resemble modern developer tooling such as Astral projects:

- extremely fast
- simple CLI
- sensible defaults
- minimal configuration
- deterministic behavior
- strong diagnostics
- excellent developer experience
- composable with existing tools

---

# Core Product Principles

## 1. Impact analysis is the product

The dependency graph is infrastructure.

Users should primarily interact with concepts such as:

- changed files
- affected modules
- affected tests
- blast radius
- dependency paths
- change risk
- validation plans

Avoid exposing graph implementation details unless they improve diagnostics or debugging.

---

## 2. Speed is a feature

The tool should feel effectively instantaneous on normal repositories.

Optimize for:

- fast initial indexing
- incremental re-indexing
- efficient reverse graph traversal
- low startup overhead
- parallel parsing where useful
- persistent caches where justified

Avoid premature micro-optimization, but architecture must not introduce obvious performance ceilings.

---

## 3. Deterministic before intelligent

Core impact analysis must be deterministic.

Do not use LLMs or probabilistic reasoning for core dependency resolution or blast-radius calculations.

If uncertainty exists because Python is dynamic, represent that uncertainty explicitly.

Example confidence levels:

- certain
- likely
- possible

Do not hide uncertainty behind a single opaque score.

---

## 4. Correctness favors recall

Urmare optimizes primarily for **impact recall**.

Missing an affected dependency or test is more harmful than including an unaffected one. Therefore:

- false positives are acceptable when needed for safety
- false negatives should be treated as the more serious failure mode
- when analysis is uncertain, prefer conservative over-selection
- uncertainty should be exposed explicitly when possible rather than hidden
- optimizations that reduce selected tests must not silently reduce confidence in correctness

The MVP only models certain/static import relationships, but its behavior should already follow this conservative principle.

---

## 5. Explain every conclusion

Whenever the system claims that entity A affects entity B, the relationship should be explainable.

For example:

```text
tests/payments/test_checkout.py
  -> imports api.checkout
  -> calls payments.service
  -> imports payments.stripe
  -> changed
```

Commands such as `why` should rely on graph paths rather than hand-written heuristics.

---

## 6. Standards first

Prefer standard Python project conventions.

First-class inputs should eventually include:

- Python source files
- `pyproject.toml`
- standard package layouts
- pytest conventions
- Git repositories

Additional integrations may include:

- uv
- Poetry
- PDM
- Hatch
- Django
- FastAPI
- Flask

Do not unnecessarily require users to adopt another build metadata format.

---

## 7. Python syntax compatibility is explicit

The initial source-syntax compatibility target is **Python 3.9 through Python 3.14**.

Urmare analyzes source code; it does not need to execute each supported Python version. The parser and import-analysis layer should be chosen and structured so repositories using valid syntax across this range can be analyzed.

Do not silently assume that the developer's locally installed Python version defines the syntax Urmare can understand.

---

## 8. Repository-relative paths are canonical

Repository-relative normalized paths are the canonical identity for user-facing and machine-readable file results.

Examples:

```text
src/payments/service.py
tests/api/test_checkout.py
```

Architectural guidance:

- use Rust path types for filesystem operations
- avoid treating arbitrary absolute path strings as stable graph identity
- normalize discovered files relative to the repository root
- keep OS-specific absolute paths at system boundaries only
- use stable repository-relative paths in JSON and human output
- ensure equivalent Windows and Unix paths resolve to the same logical repository entity

---

## 9. Portable distribution is an architectural constraint

Urmare is intended to ship as a prebuilt standalone binary across mainstream developer platforms.

The implementation should remain compatible with eventual distribution for:

- macOS on ARM64 and x86-64
- Linux on ARM64 and x86-64
- Windows on x86-64, with ARM64 support likely later
- Linux glibc environments
- Linux musl environments where practical

Users should not need Rust installed in order to use Urmare through the primary distribution channels.

When choosing dependencies or implementation techniques:

- avoid unnecessary platform-specific assumptions
- avoid unnecessary dynamically linked native dependencies
- prefer portable Rust crates when practical
- do not assume a particular Linux distribution
- do not assume `/usr/bin`, GNU-only utilities, Bash, or Unix path semantics in core logic
- keep filesystem and path handling cross-platform
- avoid designs that unnecessarily couple the binary to a specific CPython ABI

Do not add release automation during unrelated feature work. Portability should influence architecture now; packaging and release workflows belong to later implementation slices.

---

# Initial Scope

The first implementation should focus on file/module-level dependency analysis.

Primary capabilities:

1. discover Python source files
2. parse imports
3. map files to Python modules
4. build directed dependency graph
5. build reverse dependency graph
6. calculate transitive blast radius
7. discover pytest test files
8. determine affected tests
9. explain dependency paths

Initial commands:

```bash
urmare graph
urmare impact <path>
urmare tests --affected <path>
urmare why <source> <target>
```



---

# Suggested Architecture

Prefer a Rust core for performance-sensitive repository analysis.

A possible workspace structure:

```text
.
├── Cargo.toml
├── crates/
│   ├── urmare-cli/
│   ├── urmare-core/
│   ├── urmare-python/
│   └── urmare-graph/
├── fixtures/
│   └── python-projects/
├── tests/
├── product_spec.md
├── AGENTS.md
└── README.md
```

Responsibilities:

### `urmare-core`

Application-level domain models and orchestration.

Examples:

- repository
- source file
- module
- affected entity
- impact result
- confidence
- diagnostics

Avoid tying these models directly to CLI output.

### `urmare-python`

Python-specific analysis.

Responsibilities:

- source discovery
- module-path resolution
- AST parsing
- import extraction
- relative import handling
- package detection
- test discovery

### `urmare-graph`

Generic graph infrastructure.

Responsibilities:

- nodes
- typed edges
- forward traversal
- reverse traversal
- transitive closure
- shortest/explanatory paths
- connected dependency analysis

Keep this crate independent of Python semantics where possible.

### `urmare-cli`

CLI parsing and presentation.

Responsibilities:

- user commands
- human-readable output
- JSON output
- exit codes

Do not place business logic here.

---

# Graph Model

Design the graph so it can expand beyond imports.

Initial node types may include:

```text
File
Module
Test
Package
```

Future node types may include:

```text
Symbol
Class
Function
Method
APIEndpoint
Service
Job
Configuration
ExternalPackage
```

Initial edge type:

```text
IMPORTS
```

Future edges may include:

```text
CALLS
INHERITS
IMPLEMENTS
USES_TYPE
TESTS
EXPOSES
CONFIGURES
DEPLOYS
REGISTERED_AS_PLUGIN
```

Every edge should eventually be able to carry metadata such as:

```text
kind
confidence
source_location
origin
```

Example conceptual model:

```rust
struct Edge {
    from: NodeId,
    to: NodeId,
    kind: EdgeKind,
    confidence: Confidence,
    location: Option<SourceLocation>,
}
```

Avoid designing the graph exclusively around file imports.

---

# Blast Radius

Blast radius is the reverse transitive closure of a changed entity, constrained by relevant relationship types.

Given:

```text
A -> B -> C
D -> B
```

where `A -> B` means A depends on B:

if B changes, the blast radius includes:

```text
A
D
```

and potentially their dependents.

Results should distinguish:

```text
directly_affected
transitively_affected
affected_tests
```

Eventually it should also distinguish confidence.

Example:

```text
certain: 42
likely: 7
possible: 3
```

The initial implementation only needs certain/static import relationships.

---

# Test Impact

For MVP, pytest discovery can be convention-based.

Recognize files such as:

```text
test_*.py
*_test.py
```

A test is affected if:

1. it directly imports a changed module, or
2. it imports something that transitively depends on a changed module.

Do not attempt test-level function selection in the first implementation.

File-level test selection is sufficient.

Example:

```text
Changed:
src/payments/stripe.py

Affected tests:
tests/payments/test_stripe.py
tests/payments/test_service.py
tests/api/test_checkout.py
```

---

# CLI Guidelines

Commands should be obvious and composable.

Examples:

```bash
urmare impact src/foo.py
urmare impact src/foo.py --json
urmare impact --git-diff main
urmare tests --affected <path>
urmare tests --affected --git-diff origin/main
urmare why src/foo.py tests/test_bar.py
```

Avoid excessive mandatory flags.

Human output should be concise and readable.

Machine-readable JSON should be stable enough for eventual CI and agent integrations.

---

# Error Handling

Prefer actionable diagnostics.

Bad:

```text
Unable to resolve module.
```

Better:

```text
Unable to resolve import `foo.bar` from src/api/app.py:14.

Searched roots:
  src/
  .

Use `urmare graph --debug` for module resolution details.
```

Expected user mistakes should not generate Rust panics.

---

# Testing Strategy

Use fixture repositories extensively.

Create fixtures for:

- flat project layout
- `src/` layout
- packages with `__init__.py`
- namespace-style package structures where relevant
- relative imports
- nested imports
- circular dependencies
- missing imports
- test packages
- renamed/deleted files
- monorepo-style Python packages

Tests should cover both:

1. graph correctness
2. CLI behavior

Prefer small purpose-built repositories over enormous mocked AST structures.

---

# Performance Testing

Create at least one synthetic large repository fixture/generator.

Useful targets:

```text
1,000 files
10,000 files
50,000 files
```

Measure:

- discovery
- parsing
- graph construction
- impact traversal
- incremental update

Do not optimize against tiny fixtures alone.

---

# Coding Standards

For Rust:

- stable Rust
- `cargo fmt`
- `cargo clippy`
- explicit error types where they improve diagnostics
- avoid `unwrap()` in production paths
- prefer small focused modules
- document public abstractions
- benchmark performance-critical code

Dependencies should be added conservatively.

Before adding a crate, consider:

- maintenance status
- compile-time cost
- transitive dependency count
- performance
- portability and cross-compilation implications
- native/system dependencies
- whether standard library functionality is sufficient

---

# Development Rules for Agents

When implementing a feature:

1. read `product_spec.md`
2. identify the smallest coherent slice
3. update domain models first if required
4. implement logic outside CLI
5. add fixture coverage
6. add unit/integration tests
7. run formatting and linting
8. run the full test suite
9. describe architectural changes clearly

Do not refactor unrelated code unless necessary.

Do not introduce speculative abstractions without an immediate use.

Do not build future roadmap functionality prematurely.

Do not add packaging or release infrastructure unless the task explicitly asks for it.

---

# Product Boundary

Whenever a feature request starts turning the project into another tool, ask whether it belongs here.

Examples:

Running tests directly may be useful.

Replacing pytest is not.

Generating CI execution plans may be useful.

Becoming a CI provider is not.

Understanding packages is necessary.

Resolving/installing all Python dependencies is not.

Analyzing containers may eventually be useful.

Replacing Docker is not.

The engine should remain an intelligence and orchestration layer.

Container minimization belongs in a separate future Sorginte product, tentatively named **Miez** (*core / essence*), not in Urmare.


---

# Long-Term Direction

The eventual system may expose:

```text
CLI
JSON API
MCP server
CI integration
GitHub checks
editor integrations
```

Possible future capabilities:

```text
symbol-level dependency graph
function-call graph
type relationships
framework awareness
API endpoint impact
service boundaries
runtime trace enrichment
change-risk scoring
CI execution planning
agent validation planning
remote graph cache
```

These are roadmap items, not MVP requirements.

The immediate goal is simpler:

> Build the fastest, clearest way to understand what follows from a Python code change.
