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

Changed:
  src/payments/service.py

Affected:
  12 modules
  38 tests

Blast radius:
  MEDIUM
```

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
```

Urmare responds:

```text
tests/api/test_checkout.py
  -> api.checkout
  -> payments.service
  -> payments.stripe
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

## `urmare graph`

Build and inspect repository graph.

Potential output:

```text
Repository graph

Python files:        1,284
Modules:             1,231
Import edges:        7,482
Tests:                 314
Unresolved imports:     17

Indexed in 84 ms
```

---

## `urmare impact`

Example:

```bash
urmare impact src/payments/stripe.py
```

Output:

```text
Blast radius

Changed:
  src/payments/stripe.py

Direct dependents:
  4

Transitive dependents:
  21

Affected tests:
  13

Risk:
  LOW
```

Risk may initially be omitted or implemented as a very simple deterministic classification.

---

## `urmare tests --affected`

Example:

```bash
urmare tests --affected src/payments/stripe.py
```

Output:

```text
tests/payments/test_service.py
tests/payments/test_stripe.py
tests/api/test_checkout.py
```

Support `--json`.

---

## `urmare why`

Example:

```bash
urmare why src/payments/stripe.py tests/api/test_checkout.py
```

Output:

```text
tests/api/test_checkout.py
  -> src/api/checkout.py
  -> src/payments/service.py
  -> src/payments/stripe.py
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

Example output:

```text
Risk: 78 / 100
HIGH

Reasons:
  + public interface changed
  + 3 packages affected
  + 41 downstream modules
  + 2 services affected
  - strong test coverage
```

This feature is not required for the first MVP.

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

Later, optional configuration can live in:

```toml
[tool.urmare]
```

Possible future fields:

```toml
[tool.urmare]
source-roots = ["src"]
test-roots = ["tests"]
exclude = ["generated/**"]
```

Configuration should supplement inference, not replace it.

---

# JSON Output

Machine-readable output is a first-class requirement.

Example conceptual schema:

```json
{
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

Schema stability becomes increasingly important once CI/agent integrations exist.

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

Initial indexing may take longer, but repeated runs should eventually use persistent incremental state.

Do not sacrifice correctness for benchmark screenshots.

---

# Incremental Analysis

Post-MVP, Urmare should persist repository metadata.

Instead of reparsing all files:

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

# Sorginte Product Family

Urmare is the first Sorginte product.

A possible future product is **Miez** — Romanian for *core* or *essence* — focused on producing minimal application/container artifacts containing only what an application actually needs.

Miez is a separate project and explicitly outside Urmare's scope.

---

# Non-Goals for MVP

Do not implement:

- Git diff support
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
