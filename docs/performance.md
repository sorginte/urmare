# Performance methodology

Urmare's performance claims should be reproducible and tied to a documented
workload. The synthetic benchmark exercises the real repository analysis
pipeline without committing thousands of generated files to the repository.

Run it with an optimized build:

```bash
cargo bench -p urmare-core --bench synthetic
```

The benchmark creates fresh temporary repositories at two sizes:

| Python files | Source modules | Test files | Samples |
|-------------:|---------------:|-----------:|--------:|
| 1,000        | 899            | 100        | 5       |
| 10,000       | 8,999          | 1,000      | 3       |

Each repository has a conventional `src/` layout and deterministic static
imports. Production modules form a dependency chain rooted at
`src/generated/module_00000.py`; tests import modules distributed across that
chain. Changing the first module therefore creates a deliberately broad,
explainable worst-case closure containing every other production module and
every test. The benchmark validates these counts before accepting a sample.

One unrecorded uncached analysis warms filesystem caches. Repository generation
and release compilation are excluded from sample timings. Reported values are
the median and minimum of the warm samples; there is no noisy pass/fail
threshold. Cached cases use a separate temporary cache directory and validate
the exact parsed-import and graph-index hit/miss counts for every sample.

## Measured phases

- **Discovery:** filesystem traversal, repository-relative path collection,
  root canonicalization, and `pyproject.toml` configuration loading.
- **Parsing:** Python source reads and Ruff AST import extraction.
- **Graph construction:** module mapping or identity reuse, node allocation,
  local import resolution or resolved-edge reuse, provenance/trace
  materialization, and forward/reverse edge creation.
- **Complete build:** the public profiled build call, including small amounts
  of orchestration between the measured phases.
- **Impact traversal:** the public impact call, including path resolution,
  reverse closure, affected-test selection, attribution, and deterministic
  result sorting.
- **Cached no-change:** a complete build where every Python file reuses parsed
  imports, module identities, and resolved local dependencies.
- **One-file rebuild:** a complete build after changing exactly one source file;
  unchanged files reuse parsed imports and resolved dependencies, while the
  changed file is read, hashed, parsed, resolved, and persisted.

`RepositoryAnalysis::build_uncached_profiled` exposes the uncached phases from
the same pipeline used by normal analysis. Cached measurements use
`build_profiled_with_cache_directory`, which lets the benchmark isolate its
cache without changing production cache-location behavior.

## Reference observation

This is a development observation, not a cross-machine guarantee. On
2026-08-06, using Rust 1.97.1 on macOS 26.5.2 ARM64, one run produced:

| Python files | Discovery | Parsing | Graph construction | Complete build | Impact traversal |
|-------------:|----------:|--------:|-------------------:|---------------:|-----------------:|
| 1,000        | 1.149 ms  | 9.849 ms  | 1.712 ms         | 12.725 ms      | 3.212 ms         |
| 10,000       | 11.394 ms | 120.073 ms | 18.463 ms       | 150.557 ms     | 43.022 ms        |

Incremental parsed-import and graph-index results from the same run:

| Python files | Cached parsing | Cached graph | Cached no-change build | One-file parsing | One-file graph | Cache persistence | One-file rebuild |
|-------------:|---------------:|-------------:|-----------------------:|-----------------:|---------------:|------------------:|-----------------:|
| 1,000        | 1.519 ms       | 1.804 ms     | 4.620 ms               | 1.483 ms         | 1.726 ms       | 4.115 ms          | 8.727 ms         |
| 10,000       | 20.326 ms      | 21.008 ms    | 55.721 ms              | 20.215 ms        | 19.154 ms      | 10.828 ms         | 64.138 ms        |

Parsing is the dominant initial-indexing phase in this workload. Even so, the
10,000-file complete build is comfortably below the current directional warm
target of 500 ms. Urmare therefore does not add a parallel-execution dependency
in this slice. Future parallel parsing should be evaluated against this
baseline and must preserve deterministic errors and graph results.

At 10,000 files, persistent indexing reduced the observed no-change build from
150.557 ms to 55.721 ms. A one-file rebuild required one parser and edge-cache
miss, with 9,999 hits in each cache. All 10,000 module identities were reused.

Graph allocation and insertion remain whole-repository work because each CLI
invocation creates an immutable in-memory graph. On this chain-shaped workload,
reusing resolved edges does not reduce that phase by itself: cached graph
construction measured 21.008 ms versus the 18.463 ms uncached phase. Cached
records now retain candidate lists, matches, and located edge provenance so
debug and `why` results are identical on cold and warm runs. The benchmark
reports that materialization cost rather than claiming an unobserved graph
allocation speedup.

## Materializing a fixture

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
