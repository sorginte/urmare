# Urmare Roadmap

This roadmap turns Urmare's product direction into sequenced, reviewable work.
It complements the [product specification](product_spec.md), which defines the
product semantics, and the [release guide](releasing.md), which defines how a
finished version is published.

Roadmap items are ordered by dependency, not by promised delivery date. A
milestone is complete only when its behavior is documented, deterministic,
covered by offline tests, and available through the normal binary distribution.

## Current foundation

Urmare currently provides:

- deterministic file-level Python import analysis;
- direct and transitive impact calculation;
- affected pytest-file selection;
- Git-aware analysis for working-tree and merge-base changes;
- deleted-file and previous-rename identities;
- conservative full validation after configuration changes;
- dependency explanations through `urmare why`;
- a persistent incremental index;
- stable, versioned CLI JSON contracts; and
- reproducible synthetic and real-project benchmark infrastructure.

The project remains local-first. Core analysis does not require a hosted
Sorginte service, Python package installation, an LLM, or network access.

## Delivery sequence

```text
v0.3 validation plans
    ↓
repository dogfooding
    ↓
local MCP tools
    ↓
agent-specific onboarding and enforcement
    ↓
independent CI integration
    ↓
optional configurable validation execution
```

## v0.3.0 — Agent validation plans

Status: release candidate

Goal: let a terminal-capable coding agent obtain one deterministic plan after
editing Python code.

Implemented:

- [x] `urmare plan FILE...`
- [x] `urmare plan --changed`
- [x] `urmare plan --git-diff BASE`
- [x] independent plan JSON schema version 1
- [x] `selective`, `none`, and `full` validation modes
- [x] structured pytest steps with ordered repository-relative targets
- [x] existing impact attribution and full-validation semantics
- [x] complete, non-truncated human plan output
- [x] Git-root discovery and authoritative explicit roots
- [x] offline core and CLI coverage
- [x] coding-agent workflow documentation

Recommended before release:

- [ ] make `urmare plan --changed --json` usable against the Urmare repository
  itself by excluding the intentionally invalid Python syntax fixture from the
  root analysis configuration;
- [ ] verify the dogfooding workflow for a selective change, a no-test change,
  and a configuration-triggered full plan;
- [ ] merge the version change and wait for required CI on the exact release
  commit; and
- [ ] complete the protected-tag release process in
  [releasing.md](releasing.md).

Definition of done:

- the installed `urmare` binary reports version `0.3.0`;
- agents can consume the plan without parsing human output;
- every change source uses one repository analysis/update;
- failures leave JSON stdout empty and retain established exit semantics; and
- no plan executes pytest or depends on a specific coding-agent product.

## Next — Local MCP integration

Status: proposed next milestone

Goal: expose Urmare as native, read-only repository intelligence to MCP-capable
coding agents without changing analysis semantics.

Scope:

- [ ] add a local stdio MCP server distributed with Urmare;
- [ ] bind each server process to one canonical repository root;
- [ ] expose a validation-plan tool backed directly by the core
  `ValidationPlan` model;
- [ ] expose an impact-explanation tool backed by `urmare why` semantics;
- [ ] return schema-validated structured content rather than shell commands;
- [ ] share DTO conversion so CLI JSON and MCP results cannot drift;
- [ ] keep protocol traffic on stdout and diagnostics on stderr;
- [ ] reject path escapes, conflicting sources, invalid Git bases, and roots
  outside the server boundary;
- [ ] document local installation for supported MCP clients; and
- [ ] add offline protocol, lifecycle, schema, error, cache, and packaging tests.

Definition of done:

- an agent can discover and call Urmare tools without constructing a terminal
  command;
- the MCP plan is semantically identical to `urmare plan --json`;
- the server never modifies the analyzed repository;
- warm calls preserve bounded incremental behavior; and
- Codex, Claude Code, and Cursor setup is verified without claiming deeper
  native integration than each client provides.

Not included in this milestone:

- remote or hosted MCP transport;
- authentication or organization accounts;
- pytest execution;
- arbitrary shell commands; or
- LLM-generated impact relationships.

## Next — Agent onboarding and enforcement

Status: follows local MCP

Goal: make correct use of the validation plan repeatable instead of relying on
an agent to infer the workflow.

Scope:

- [ ] publish a vendor-neutral instruction template;
- [ ] provide reviewed Codex, Claude Code, and Cursor configuration examples;
- [ ] require plan recomputation after further code edits;
- [ ] require every returned validation step to succeed before completion;
- [ ] direct agents to `urmare why` when an affected relationship needs
  investigation;
- [ ] document permission and trust boundaries for tool and terminal calls;
  and
- [ ] evaluate vendor-specific hooks or plugins for pre-completion enforcement.

Definition of done:

- a new contributor can install Urmare, connect it to a supported agent, and
  follow one documented workflow;
- instructions distinguish planning from execution;
- no integration silently grants unrestricted command execution; and
- agent output does not replace independent CI validation.

## Later — CI integration

Status: planned direction; interface not yet committed

Goal: let CI independently recompute and enforce the same validation plan used
by agents.

Potential scope:

- [ ] define a CI-facing plan-consumption contract;
- [ ] run every structured validation step through the repository's configured
  environment;
- [ ] preserve plan and validation results as reviewable artifacts;
- [ ] report selective, none, and full decisions clearly;
- [ ] fail closed when planning or execution is incomplete;
- [ ] evaluate a small GitHub Action after the portable CI contract is stable;
  and
- [ ] keep the underlying CLI usable in any CI provider.

CI must recompute the plan from the checked-out commit and comparison base. It
must not trust a plan supplied only by a coding agent.

## Later — Configurable validation execution

Status: exploratory; requires a separate product decision

Today Urmare plans pytest targets but does not execute them. Agents and CI map
the structured `pytest` step into the repository's established environment,
such as `pytest`, `uv run pytest`, Poetry, tox, or a container workflow.

Before Urmare executes validation itself, the project must specify:

- a deterministic, reviewable configuration format;
- argument-safe structured invocation without shell interpolation;
- supported runner and environment boundaries;
- permission, timeout, cancellation, and signal behavior;
- exit-status and partial-result semantics;
- interaction with full-validation mode; and
- portability across macOS, Linux, and Windows.

This work must not turn Urmare into a test framework, package manager, build
system, or CI provider.

## Later — Analysis coverage

Status: long-term product work

Candidate improvements, in priority order:

1. improve static import coverage while preserving explainability;
2. represent uncertain Python relationships explicitly;
3. model common re-export and package-boundary behavior;
4. add framework-aware relationships only where they remain deterministic;
5. evaluate symbol-level impact after file-level recall is well measured; and
6. add broader validation kinds only after the pytest plan contract is stable.

Every precision improvement must be evaluated primarily against impact recall.
An optimization must not silently omit affected code or tests.

## Ongoing engineering work

These concerns apply to every milestone:

- preserve deterministic repository-relative output;
- maintain bounded warm and incremental behavior;
- keep normal tests offline;
- retain macOS, Linux, and Windows portability;
- preserve the Rust 1.95 minimum supported version until deliberately changed;
- test recovery from missing, corrupt, locked, or incompatible indexes;
- keep release artifacts reproducible and attributable to one commit; and
- document performance observations without turning them into guarantees.

## Explicitly outside the roadmap

Urmare is not planning to become:

- an LLM or RAG system;
- a generic coding-agent chat interface;
- a pytest replacement;
- a Python package manager or build system;
- a CI provider;
- a hosted-service requirement for core analysis;
- a multi-language analyzer before Python impact analysis is mature; or
- a container minimization product.

Container minimization remains the boundary of the separate potential Sorginte
product described as Miez in the product specification.

## Maintaining this roadmap

Update this document when a milestone is accepted, completed, deferred, or
split. A checked item should correspond to implemented behavior with passing
tests; mark a milestone released only after its tagged release is public.
Detailed product semantics belong in `product_spec.md`; operational release
steps belong in `releasing.md`; benchmark methodology belongs in
`performance.md`.
