# Performance methodology

Urmare uses two distinct benchmark families. Synthetic 1K/10K/50K repositories
demonstrate controlled scaling and bounded work. A pinned public-repository
corpus demonstrates behavior on real project layouts. Neither suite substitutes
for the other, and wall-clock results are observations rather than portable
pass/fail thresholds.

## Synthetic benchmark

Run the optimized benchmark with:

```bash
cargo bench -p urmare-core --bench synthetic --locked
```

The default run creates temporary 1,000- and 10,000-file Git repositories. The
50,000-file case is intentionally opt-in:

```bash
URMARE_BENCH_50000=1 cargo bench -p urmare-core --bench synthetic --locked
```

No generated large repository is checked in. Repository generation, Git
fixture setup, mutations, and fresh-build correctness comparisons are excluded
from reported timings.

### Workload

Ten percent of each repository's Python files are pytest-style tests. One file
is `src/generated/__init__.py`; the remaining production modules form a chain
rooted at `src/generated/module_00000.py`. Tests import modules distributed
across that chain. Impact from `module_00000.py` is therefore a deliberately
broad closure, while impact from the final module is narrow.

Each size runs these state transitions in order:

1. cold full persistent-index creation;
2. clean no-change reuse;
3. one changed file with identical parsed imports;
4. one changed file adding an unresolved import;
5. addition of the candidate module that satisfies that import;
6. deletion of that candidate module;
7. rename of one module;
8. configuration-invalidated full rebuild;
9. narrow impact traversal;
10. broad impact traversal.

Before accepting each measured update, the benchmark asserts its expected work
counts. After every mutation it builds a fresh uncached analysis and compares
summary, unresolved diagnostics, narrow impact, and broad impact. Profiled
impact is also compared with a fresh result before being reported.

The differential integration suite goes further than the benchmark: it
compares complete module mappings, forward and reverse relationships,
provenance, resolution candidates/matches, unresolved locations, every
single-file impact and affected-test result, and ordinary `why` outcomes after
each deterministic mutation.

### Measurements and counters

The benchmark reports complete public build time and separately exposes:

- persistent index load;
- Git delta detection;
- update planning and parsing/resolution work;
- transactional persistence;
- Python files parsed;
- importers re-resolved;
- persistent records written;
- serialized bytes inserted or replaced.

Deletion removes point records and therefore increments the record counter but
does not add serialized bytes. Reverse edges and candidate-to-importer
relationships are stored as individual point records; bytes rewritten do not
include unrelated members of a large adjacency or candidate set.

`RepositoryAnalysis::impact_profiled` and `why_profiled` report index-open
time, query time, recovery-rebuild time when applicable, and persistent records
read. These values are internal instrumentation and do not change the CLI or
JSON contracts.

Work counts are more portable than timings. Required invariants include:

```text
no change             0 parses, 0 resolutions, 0 edge changes, 0 writes
content-only edit     1 parse, 0 resolutions, 0 edge changes
import edit           1 parse, only that importer re-resolved
candidate add/remove  only candidate-dependent importers re-resolved
configuration change complete rebuild with explicit fallback reason
```

### Storage decision

The repository index uses redb 4.2. Its packaged documentation describes it as
stable and maintained, pure Rust, ACID, crash safe, and capable of concurrent
transactional readers with a writer. The crate's Rust minimum is 1.90, below
Urmare's 1.95 floor, and it has no production transitive dependency. It adds no
native library, system database, runtime service, or target-specific build
requirement, which keeps Windows, macOS, Linux, and cross-compilation practical.

The alternative of one JSON snapshot was rejected because updating one record
would deserialize and rewrite the complete repository. The implemented schema
uses independent records and transactional commits. A versioned snapshot plus
journal was not needed because redb already provides point updates, rollback,
atomic commit, and corruption detection in one portable dependency.

## Real-project benchmark methodology

The real-project suite is publication infrastructure. It is driven by
`benchmarks/real_projects/benchmark.py` and the reviewed, machine-readable
`benchmarks/real_projects/corpus.json`. The runner uses only the Python standard
library. It never contacts a hosted Sorginte service. The reviewed reference
observation below follows this methodology.

### Corpus and file selection

The corpus was selected on 2026-09-02 from recent stable releases in the five
official repositories. Every revision is a full commit SHA behind the recorded
release tag. The changed files were selected before observing benchmark timings:

| Project | Release | Full pinned commit | Predetermined changed file |
|---|---|---|---|
| Flask | 3.1.3 | `22d924701a6ae2e4cd01e9a15bbaf3946094af65` | `src/flask/app.py` |
| FastAPI | 0.136.3 | `82064857539e6286522c347b4b11331b48dd2378` | `fastapi/routing.py` |
| Django | 6.1 | `fe0a859f537d4238cf49fca39073513206f83122` | `django/db/models/query.py` |
| pandas | 3.0.5 | `e68db09ecf6427d1b62e565bacf17f2e525a3032` | `pandas/core/frame.py` |
| Apache Airflow | 3.3.1 | `3adbbe1c58e4532df1964cb7794805e763816ee8` | `airflow-core/src/airflow/models/dag.py` |

Each file is a stable, non-generated production module in the primary package
and has a meaningful static dependent where practical. Selection favors
architectural representativeness, not a favorable timing or blast radius. The
manifest records the rationale, exact command, minimum repository-scale checks,
and preparation policy for every project.

Django's test-runner suite contains one file whose purpose is to be invalid
Python. The reviewed `config/django.toml` patch excludes only
`tests/test_runner_apps/tagged/tests_syntax_error.py`; its upstream and patched
`pyproject.toml` hashes are recorded just like Airflow's configuration. This
keeps an intentional error fixture from being treated as repository source.

Airflow requires explicit configuration because it is a monorepo with multiple
independent import roots. The runner appends the reviewed
`benchmarks/real_projects/config/airflow.toml` to the pinned root
`pyproject.toml`. Both the upstream file and configured result are SHA-256
verified. The configuration lists Airflow core, task SDK, CLI, development
helpers, and shared package roots and test roots; excludes the duplicate task
SDK namespace initializer; and excludes provider, generated, documentation,
and tooling trees outside the benchmarked core distribution. The patch is
identical for every Airflow sample.

### Preparation and isolation

`prepare` is the only command that needs network access. It creates a shallow
bare mirror containing the complete pinned commit, then verifies the official
origin, tag resolution, selected file, and configuration hashes. A prepared
measurement run uses only local mirrors and therefore can run with networking
disabled. Ordinary unit, integration, dry-run, smoke, and summary tests never
clone public repositories.

Every paired sample gets a new local clone at the exact commit. The runner
applies any reviewed patch and records it in a deterministic benchmark-only Git
commit with the pinned upstream commit as its sole parent. This keeps the
measured checkout clean, which is required for no-change reuse, without changing
the mirror or obscuring the upstream identity. The runner verifies the parent,
exact one-file commit diff, configuration hash, selected file, clean Git status,
and minimum Python/test counts before timing. The public CLI receives isolated
platform cache locations below the sample directory. The profiling helper uses
a different explicit cache below the same sample directory, so profiling cannot
warm or modify the timed CLI cache. A completed sample directory is destroyed;
a failed one is moved only within the benchmark-owned quarantine directory.
Containment checks prevent recursive deletion outside the ownership-marked work
root.

The same absolute optimized `urmare` executable is used for every project and
sample. Raw records include its SHA-256, version, absolute path, and the Urmare
Git commit. The release-mode profiling helper embeds its build commit and Rust
compiler version; the runner rejects a helper from another commit. Official
runs also reject a dirty Urmare source tree.

### Paired sample lifecycle

The default is 15 independent paired samples per project. Each sample performs:

1. setup, exact checkout/configuration verification, scale validation, and a
   fresh correctness analysis outside measured intervals;
2. a measured cold invocation with an empty Urmare cache;
3. one discarded warm-up invocation using the populated index;
4. a measured warm no-change invocation;
5. a deterministic comment-only edit to the selected file, preserving imports;
6. a measured one-file incremental invocation;
7. bounded-work assertions and comparison with a fresh uncached analysis.

"Cold" means no persistent Urmare index exists; the runner does not claim to
flush operating-system filesystem caches. "Warm" means the committed index
already represents the unchanged checkout. "Incremental" means the index
represents the original checkout and exactly one selected file receives the
recorded content-only edit. Setup, local cloning, configuration, scale checks,
fixture edits, fresh correctness builds, and cleanup are excluded from measured
CLI intervals.

The incremental profile must parse and read exactly one file, perform no module
or edge changes, re-resolve no importers, write at most two persistent records,
and report no fallback. Warm profiles must report reuse with zero
parsing, hashing, resolution, or writes. Failure of the command, JSON contract,
revision/configuration checks, cache isolation, scale floor, bounded-work
checks, or fresh-result comparison invalidates the sample and stops the run.

### End-to-end and internal measurement layers

End-to-end latency surrounds the exact command:

```text
urmare --root REPOSITORY impact CHANGED_FILE --json
```

It starts immediately before process invocation and ends after process exit,
including startup, argument parsing, root handling, index work, impact query,
serialization, and output. Exit status, stdout, and stderr are retained.

Internal profiling is performed separately, outside that interval, by the
release-mode `profile_repository` example using Urmare's public profiling APIs
and its own cache. It reports index load, Git delta detection, update,
persistence, index open, impact query, recovery fallback, file/work/edge/record
counters, bytes written, build kind, and fallback reason. Internal phase totals
must never be labeled as end-to-end CLI latency.

### Raw data, correctness, and statistics

Raw output is schema-versioned JSON Lines with its version-1 contract in
`benchmarks/real_projects/raw-result.schema.json`. Every successful cold,
discarded warm-up, measured warm, and measured incremental invocation is
appended and fsynced independently. A truncated line is not a sample. Resume is
allowed only when every existing paired sample has exactly the four expected
lifecycle records; ambiguous partial samples are refused rather than guessed or
silently repeated.

Records include project/revision/file identity, sample/scenario, exact command,
binary and build provenance, UTC timestamp, OS/version/architecture, CPU/core
and available memory metadata, Git and Rust versions, end-to-end time, internal
timings and counters, repository and affected-result counts, exit state, and a
SHA-256 of canonicalized impact JSON. They do not record arbitrary environment
variables, usernames, secrets, cache paths, or checkout paths.

The incremental persistent result must match a fresh uncached result. Because
the edit preserves imports, cold, warm, and incremental normalized result hashes
must also match within the paired sample.

The separate `summarize` command never modifies this file or `README.md`. It
reads all raw observations, excludes records marked as warm-up, and emits a
proposed Markdown or JSON summary. For each project/scenario it reports sample
count, median, linearly interpolated p25/p75, p95, minimum, and maximum.
End-to-end CLI, internal index/update, impact-query, internal query-total, and
work-counter summaries are visibly separate. Raw observations remain the source
of truth.

### Commands

Build the one CLI binary and the profiling helper from the same clean commit:

```bash
cargo build --release --locked -p urmare-cli --bin urmare
cargo build --release --locked -p urmare-core --example profile_repository
```

Prepare, inspect, run, and summarize the official suite:

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

Use `--project ID` to prepare, inspect, or run one project. Pass `--resume` only
for an output whose existing samples are complete. The offline validation path
uses the same lifecycle without a public clone:

```bash
python3 benchmarks/real_projects/benchmark.py smoke \
  --work-dir target/real-project-smoke \
  --binary target/release/urmare \
  --profile-helper target/release/examples/profile_repository \
  --output target/real-project-smoke/raw.jsonl
```

### Known limitations

- Cold runs isolate Urmare's persistent cache but do not flush the OS page
  cache or control CPU frequency, thermal state, or other host load.
- Process timing uses the host's monotonic high-resolution clock; cross-machine
  comparisons require review of the recorded hardware and software metadata.
- The helper reproduces each scenario on a parallel isolated cache. Its internal
  timings describe the same core operation but are not components of the timed
  CLI process.
- The corpus covers five substantial projects at pinned revisions, not every
  Python layout, import pattern, operating system, or filesystem.
- Static file-level import analysis and the explicit Airflow core boundary
  remain subject to the product limitations documented below.

## Real-project reference observation

This is a development observation from the v0.2.0 incremental-indexing branch,
not a cross-machine guarantee. It was recorded on 2026-09-03 from Urmare commit
`daaf7a592bb54ef0ee71b1ed3065caaf9187d67b` with one release-mode binary. The
package metadata still reported version 0.1.1 because the benchmark work did
not change package versions; the full Git commit is the authoritative source
identity for this observation.

The host was an Apple M5 Pro with 18 logical cores and 48 GiB of memory, running
macOS 26.6.2 on ARM64. The helper was built with
`rustc 1.97.1 (8bab26f4f 2026-07-14)`, and Git was
`2.50.1 (Apple Git-155)`. The binary SHA-256 was
`d7c01ae163fabc060bec0d6681b67a55ccb914261e8011a0a554c5185be4b774`.

Run `c9aaceb78e6075b04a68fa50` contains 15 independently prepared paired
samples per project: 225 measured scenario records and 75 explicitly discarded
warm-up records. It ran from 2026-09-03 07:26:54Z through 07:31:25Z. The
preserved 81,689,420-byte JSON Lines artifact is
`target/real-project-benchmark/raw/official-daaf7a592bb5-complete-metadata.jsonl`;
its SHA-256 is
`650b5bc82196b93bf2c92c9173ece8d00c1efc08292caf8c1041d5e1b670ec26`.
The deterministic Markdown and JSON summaries are in the adjacent
`target/real-project-benchmark/summary/` directory. These generated artifacts
are benchmark outputs and are intentionally ignored by Git; retain or publish
the raw artifact with any external use of these results.

All latency values below are milliseconds. `IQR` is p25-p75. The end-to-end
table measures the exact CLI process and is not calculated from the internal
phase totals.

### End-to-end CLI latency

| Project | Scenario | n | Median | IQR | p95 | Min-max |
|---|---|---:|---:|---:|---:|---:|
| Flask | Cold | 15 | 115.883 | 114.511-117.665 | 124.157 | 113.389-127.300 |
| Flask | Warm | 15 | 75.254 | 73.872-76.500 | 78.671 | 72.856-79.113 |
| Flask | Incremental | 15 | 78.609 | 76.457-81.980 | 85.447 | 75.957-86.007 |
| FastAPI | Cold | 15 | 221.809 | 217.900-224.489 | 298.061 | 208.141-298.436 |
| FastAPI | Warm | 15 | 91.923 | 90.624-92.701 | 172.271 | 90.384-177.762 |
| FastAPI | Incremental | 15 | 100.880 | 99.035-102.862 | 109.857 | 95.588-112.802 |
| Django | Cold | 15 | 628.448 | 624.825-634.940 | 703.457 | 618.037-706.696 |
| Django | Warm | 15 | 220.379 | 219.920-222.269 | 298.505 | 218.368-329.329 |
| Django | Incremental | 15 | 230.289 | 229.862-232.105 | 241.253 | 226.750-242.308 |
| pandas | Cold | 15 | 456.446 | 452.262-463.127 | 557.454 | 448.502-629.756 |
| pandas | Warm | 15 | 117.072 | 115.142-118.067 | 195.405 | 112.467-271.490 |
| pandas | Incremental | 15 | 125.998 | 124.716-126.838 | 131.451 | 122.195-135.358 |
| Apache Airflow | Cold | 15 | 610.023 | 599.653-669.220 | 694.693 | 581.647-695.181 |
| Apache Airflow | Warm | 15 | 255.853 | 253.611-320.491 | 346.366 | 249.142-356.443 |
| Apache Airflow | Incremental | 15 | 262.839 | 260.167-264.596 | 265.972 | 259.225-266.349 |

### Internal index/update latency

This separately profiled layer includes index load, Git delta detection, update
planning/execution, and persistence. It is not a component measurement from the
timed CLI process.

| Project | Scenario | n | Median | IQR | p95 | Min-max |
|---|---|---:|---:|---:|---:|---:|
| Flask | Cold | 15 | 38.204 | 37.136-38.772 | 41.009 | 33.645-41.542 |
| Flask | Warm | 15 | 60.225 | 59.822-61.129 | 65.647 | 58.948-72.154 |
| Flask | Incremental | 15 | 65.039 | 62.047-66.884 | 77.132 | 59.149-79.706 |
| FastAPI | Cold | 15 | 103.975 | 103.004-106.316 | 116.242 | 100.278-118.628 |
| FastAPI | Warm | 15 | 70.119 | 69.060-71.146 | 151.223 | 68.382-156.239 |
| FastAPI | Incremental | 15 | 78.591 | 77.094-79.037 | 83.276 | 75.678-90.403 |
| Django | Cold | 15 | 373.985 | 371.437-378.818 | 384.684 | 368.656-390.365 |
| Django | Warm | 15 | 167.282 | 166.087-171.281 | 243.484 | 160.417-257.309 |
| Django | Incremental | 15 | 169.082 | 167.553-169.638 | 173.787 | 164.525-180.994 |
| pandas | Cold | 15 | 321.298 | 317.873-323.043 | 327.717 | 316.182-329.285 |
| pandas | Warm | 15 | 67.046 | 66.186-71.293 | 149.487 | 64.805-226.283 |
| pandas | Incremental | 15 | 78.865 | 77.828-80.567 | 82.191 | 75.303-83.007 |
| Apache Airflow | Cold | 15 | 313.768 | 310.996-315.696 | 323.811 | 309.190-324.704 |
| Apache Airflow | Warm | 15 | 204.313 | 199.126-262.273 | 287.814 | 196.885-293.286 |
| Apache Airflow | Incremental | 15 | 205.749 | 205.077-207.629 | 215.459 | 202.938-216.966 |

### Internal impact-query latency

| Project | Scenario | n | Median | IQR | p95 | Min-max |
|---|---|---:|---:|---:|---:|---:|
| Flask | Cold | 15 | 1.093 | 1.074-1.109 | 1.132 | 1.060-1.145 |
| Flask | Warm | 15 | 1.113 | 1.098-1.145 | 1.162 | 1.088-1.172 |
| Flask | Incremental | 15 | 1.119 | 1.104-1.130 | 1.153 | 1.057-1.164 |
| FastAPI | Cold | 15 | 7.555 | 7.466-7.779 | 9.611 | 7.268-9.661 |
| FastAPI | Warm | 15 | 7.350 | 7.325-7.417 | 7.643 | 7.166-7.694 |
| FastAPI | Incremental | 15 | 7.429 | 7.290-7.496 | 7.662 | 7.216-7.786 |
| Django | Cold | 15 | 43.339 | 43.103-43.710 | 44.242 | 42.606-44.471 |
| Django | Warm | 15 | 43.621 | 43.354-43.728 | 50.332 | 42.954-50.643 |
| Django | Incremental | 15 | 43.268 | 43.050-43.719 | 43.943 | 42.829-43.952 |
| pandas | Cold | 15 | 32.075 | 31.737-32.265 | 32.592 | 31.494-32.761 |
| pandas | Warm | 15 | 31.558 | 31.419-31.701 | 31.995 | 31.262-32.192 |
| pandas | Incremental | 15 | 31.512 | 31.353-31.672 | 33.961 | 31.002-37.277 |
| Apache Airflow | Cold | 15 | 40.572 | 40.469-40.763 | 43.482 | 40.159-47.348 |
| Apache Airflow | Warm | 15 | 40.445 | 40.246-40.734 | 41.063 | 40.027-41.129 |
| Apache Airflow | Incremental | 15 | 40.397 | 40.324-40.681 | 41.017 | 40.099-41.026 |

### Deterministic work and result validation

The work counters did not vary within any project/scenario group, so the table
reports their exact medians. `Edges +/−` shows forward-edge mutations; reverse
edge additions matched forward-edge additions for cold builds, and both
directions had zero mutations for warm and content-only incremental builds.

| Project | Scenario | Parsed | Hashed | Re-resolved | Edges +/− | Records written | Bytes written |
|---|---|---:|---:|---:|---:|---:|---:|
| Flask | Cold | 83 | 83 | 83 | 204/0 | 1,469 | 382,825 |
| Flask | Warm | 0 | 0 | 0 | 0/0 | 0 | 0 |
| Flask | Incremental | 1 | 1 | 0 | 0/0 | 2 | 32,724 |
| FastAPI | Cold | 1,118 | 1,118 | 1,118 | 2,156/0 | 12,421 | 3,317,209 |
| FastAPI | Warm | 0 | 0 | 0 | 0/0 | 0 | 0 |
| FastAPI | Incremental | 1 | 1 | 0 | 0/0 | 2 | 44,123 |
| Django | Cold | 2,916 | 2,916 | 2,916 | 16,159/0 | 57,906 | 19,224,495 |
| Django | Warm | 0 | 0 | 0 | 0/0 | 0 | 0 |
| Django | Incremental | 1 | 1 | 0 | 0/0 | 2 | 40,759 |
| pandas | Cold | 1,514 | 1,514 | 1,514 | 8,349/0 | 38,557 | 13,872,030 |
| pandas | Warm | 0 | 0 | 0 | 0/0 | 0 | 0 |
| pandas | Incremental | 1 | 1 | 0 | 0/0 | 2 | 190,980 |
| Apache Airflow | Cold | 1,716 | 1,716 | 1,716 | 11,973/0 | 56,969 | 22,625,782 |
| Apache Airflow | Warm | 0 | 0 | 0 | 0/0 | 0 | 0 |
| Apache Airflow | Incremental | 1 | 1 | 0 | 0/0 | 2 | 55,422 |

Every project produced one normalized result hash across all 45 measured
records, proving that cold, warm, and content-only incremental results agreed.
The affected counts exclude affected tests from the module count.

| Project | Python files | Test files | Direct modules | Transitive modules | Affected modules | Affected tests | Normalized result SHA-256 |
|---|---:|---:|---:|---:|---:|---:|---|
| Flask | 83 | 27 | 7 | 37 | 44 | 24 | `7fbe9ecca5096a4190f887fb4ef2f256961282b45d3720a26c51e3648c2e9581` |
| FastAPI | 1,118 | 498 | 11 | 354 | 365 | 467 | `a1223c8cc40069eaf118f598a7f908734f34880bdcaf0fab7b530381dcef377a` |
| Django | 2,916 | 627 | 12 | 1,476 | 1,488 | 624 | `f62f1a528e049b1995716a91438c2bbce491b9563c24d79514b907831ef04e18` |
| pandas | 1,514 | 972 | 18 | 364 | 382 | 960 | `4690af8d4acdac821ca06922dd70c30ad55c5485a42202f66a82cc260493dc63` |
| Apache Airflow | 1,716 | 686 | 36 | 689 | 725 | 494 | `b2069f52c8c903eade08b332579302e9191d2edf6d772e003501a0a561c9e227` |

The warm and incremental medians are intentionally reported independently.
A content-only edit adds one stat/read/hash/parse and two point-record writes;
it is not expected to be faster than a no-change invocation. Higher p95/max
values in several groups illustrate the host-noise limitation above and are
retained rather than filtered.

## Synthetic reference observation

This is a development observation, not a cross-machine guarantee. On
2026-08-27, using Rust 1.97.1 on macOS 26.5.2 ARM64, one optimized run produced:

### 1,000 Python files

| Operation | Total | Git delta | Update | Persist | Parses | Re-resolves | Records | Bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Cold full index | 116.161 ms | 0.000 ms | 16.871 ms | 17.589 ms | 1,000 | 1,000 | 5,993 | 1,264,955 |
| Warm no-change | 53.473 ms | 40.272 ms | 0.010 ms | 0.000 ms | 0 | 0 | 0 | 0 |
| Content-only edit | 57.664 ms | 40.867 ms | 0.082 ms | 3.920 ms | 1 | 0 | 2 | 1,691 |
| Import edit | 63.453 ms | 46.838 ms | 0.163 ms | 3.866 ms | 1 | 1 | 3 | 2,022 |
| Candidate module add | 59.846 ms | 42.128 ms | 0.118 ms | 3.637 ms | 1 | 2 | 5 | 2,536 |
| Candidate module delete | 59.403 ms | 41.341 ms | 0.087 ms | 3.623 ms | 0 | 1 | 5 | 1,975 |
| Module rename | 60.612 ms | 40.704 ms | 0.103 ms | 3.305 ms | 1 | 1 | 15 | 2,404 |
| Configuration rebuild | 126.926 ms | 41.991 ms | 14.487 ms | 16.028 ms | 1,000 | 1,000 | 5,994 | 1,265,455 |

| Query | Total | Records read | Affected result files |
|---|---:|---:|---:|
| Narrow impact | 0.035 ms | 3 | 0 |
| Broad impact | 3.402 ms | 2,001 | 998 |

### 10,000 Python files

| Operation | Total | Git delta | Update | Persist | Parses | Re-resolves | Records | Bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Cold full index | 420.897 ms | 0.000 ms | 223.097 ms | 84.717 ms | 10,000 | 10,000 | 59,993 | 12,659,860 |
| Warm no-change | 83.481 ms | 68.382 ms | 0.014 ms | 0.000 ms | 0 | 0 | 0 | 0 |
| Content-only edit | 84.554 ms | 65.825 ms | 0.101 ms | 5.603 ms | 1 | 0 | 2 | 1,696 |
| Import edit | 130.793 ms | 68.098 ms | 0.126 ms | 6.216 ms | 1 | 1 | 3 | 2,027 |
| Candidate module add | 102.519 ms | 55.162 ms | 0.137 ms | 4.732 ms | 1 | 2 | 5 | 2,541 |
| Candidate module delete | 74.438 ms | 54.081 ms | 0.106 ms | 6.101 ms | 0 | 1 | 5 | 1,980 |
| Module rename | 75.745 ms | 58.120 ms | 0.123 ms | 3.938 ms | 1 | 1 | 15 | 2,409 |
| Configuration rebuild | 362.602 ms | 54.221 ms | 159.582 ms | 77.466 ms | 10,000 | 10,000 | 59,994 | 12,660,360 |

| Query | Total | Records read | Affected result files |
|---|---:|---:|---:|
| Narrow impact | 0.100 ms | 3 | 0 |
| Broad impact | 35.561 ms | 20,001 | 9,998 |

### 50,000 Python files (opt-in)

| Operation | Total | Git delta | Update | Persist | Parses | Re-resolves | Records | Bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Cold full index | 2,905.084 ms | 0.000 ms | 2,381.818 ms | 311.954 ms | 50,000 | 50,000 | 299,993 | 63,303,860 |
| Warm no-change | 134.538 ms | 121.012 ms | 0.015 ms | 0.000 ms | 0 | 0 | 0 | 0 |
| Content-only edit | 137.846 ms | 121.262 ms | 0.163 ms | 3.602 ms | 1 | 0 | 2 | 1,696 |
| Import edit | 164.269 ms | 145.221 ms | 0.194 ms | 3.739 ms | 1 | 1 | 3 | 2,027 |
| Candidate module add | 154.330 ms | 135.761 ms | 0.210 ms | 3.719 ms | 1 | 2 | 5 | 2,541 |
| Candidate module delete | 241.406 ms | 156.755 ms | 0.200 ms | 6.465 ms | 0 | 1 | 5 | 1,980 |
| Module rename | 200.323 ms | 142.385 ms | 0.194 ms | 11.293 ms | 1 | 1 | 15 | 2,409 |
| Configuration rebuild | 1,903.471 ms | 116.675 ms | 1,018.640 ms | 576.501 ms | 50,000 | 50,000 | 299,994 | 63,304,360 |

| Query | Total | Records read | Affected result files |
|---|---:|---:|---:|
| Narrow impact | 0.298 ms | 3 | 0 |
| Broad impact | 188.246 ms | 100,001 | 49,998 |

### Before and after

The previous cache-assisted implementation still discovered every Python file,
mapped every module, allocated every graph node, and materialized every forward
and reverse edge on each command. On the same development machine, its earlier
10,000-file observation reported 55.721 ms for a cached no-change rebuild and
64.138 ms for a one-file rebuild.

The new 10,000-file no-change observation is 83.481 ms, so this run does not demonstrate a
wall-clock latency win. It demonstrates a work-scaling change: zero Python
files parsed, zero importers resolved, zero graph relationships mutated, and
zero index records written, instead of complete discovery and graph
reconstruction. Git delta detection accounts for 68.382 ms of the observed
83.481 ms and is the clearest remaining latency target. In particular, the
current correctness check for `assume-unchanged` and `skip-worktree` inspects
tracked Python index entries, so delta detection is not yet strictly bounded
by the changed path count.

The ignored-file query excludes ordinary discovery's built-in environment and
tool-cache directories through negative Git pathspecs, so Python files inside
an ignored `.venv`, `venv`, `.tox`, or cache tree are not returned into the
delta inventory. Configured `tool.urmare.exclude` globs are applied after Git
returns ignored paths; Git may therefore still spend time enumerating a large
ignored tree that is pruned only by a custom configuration pattern, even
though those paths do not proceed to file stats, parsing, or index updates.

For a 10,000-file content-only edit, the new implementation parsed one file,
performed zero import resolutions or edge changes, and wrote two records
(1,696 bytes). Candidate add/delete re-resolved only one dependent importer
plus the new module where applicable. A rename wrote 15 point records and
2,409 bytes; it did not rewrite the 10,000-entry shared package candidate set.

These numbers prove bounded work for this deterministic Git workload and exact
agreement with fresh analysis for the compared outputs. They do not prove that
every filesystem or repository operation is incremental, predict timings on
other hardware, cover dynamic Python semantics, or establish a non-Git fast
path.

## Conservative boundaries

The fast path depends on a Git top-level repository and a compatible index.
Non-Git roots, relevant configuration changes, source-root remapping, parser or
resolver changes, schema incompatibility, unavailable Git state, and unsafe
storage conditions use a complete correct fallback. Complete graph/debug and
unresolved output remains proportional to its complete output. Broad impact is
necessarily proportional to its affected closure.

## Materializing a synthetic fixture

Contributors can generate a repository for profiling the CLI or another tool:

```bash
cargo run -p urmare-core --example generate_synthetic -- \
  target/synthetic-1000 1000

cargo run -p urmare-cli -- \
  --root target/synthetic-1000 \
  impact src/generated/module_00000.py
```

The destination must be absent or empty. The generator never deletes or
overwrites existing files.
