#[path = "../benchmarking/synthetic_repository.rs"]
mod synthetic_repository;

use std::error::Error;
use std::fs::OpenOptions;
use std::hint::black_box;
use std::io::Write;
use std::time::{Duration, Instant};

use synthetic_repository::{SyntheticRepository, generate};
use tempfile::tempdir;
use urmare_core::{AnalysisTimings, RepositoryAnalysis};

const CASES: &[(usize, usize)] = &[(1_000, 5), (10_000, 3)];

fn main() -> Result<(), Box<dyn Error>> {
    println!("Urmare synthetic warm-performance baseline");
    println!("Generated repositories are deterministic and excluded from sample timing.\n");

    for &(file_count, samples) in CASES {
        run_case(file_count, samples)?;
    }
    Ok(())
}

fn run_case(file_count: usize, samples: usize) -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let fixture = generate(directory.path(), file_count)?;

    // Populate filesystem caches before recording warm measurements.
    let (warm_repository, _) = RepositoryAnalysis::build_uncached_profiled(directory.path())?;
    validate_analysis(&warm_repository, &fixture)?;
    black_box(warm_repository.impact(&fixture.changed_file)?);

    let mut discovery = Vec::with_capacity(samples);
    let mut parsing = Vec::with_capacity(samples);
    let mut graph_construction = Vec::with_capacity(samples);
    let mut complete_build = Vec::with_capacity(samples);
    let mut impact = Vec::with_capacity(samples);

    for _ in 0..samples {
        let build_started = Instant::now();
        let (repository, timings) = RepositoryAnalysis::build_uncached_profiled(directory.path())?;
        complete_build.push(build_started.elapsed());
        record_timings(
            timings,
            &mut discovery,
            &mut parsing,
            &mut graph_construction,
        );
        validate_analysis(&repository, &fixture)?;

        let impact_started = Instant::now();
        let result = repository.impact(&fixture.changed_file)?;
        impact.push(impact_started.elapsed());
        validate_impact(&result, &fixture)?;
        black_box(result);
    }

    let cache_directory = tempdir()?;
    let (cold_cached, cold_timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
        directory.path(),
        cache_directory.path(),
    )?;
    validate_analysis(&cold_cached, &fixture)?;
    validate_cache_counts(cold_timings, 0, file_count, 0, 0, file_count)?;

    let mut cached_parsing = Vec::with_capacity(samples);
    let mut cached_graph = Vec::with_capacity(samples);
    let mut cached_build = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let (repository, timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
            directory.path(),
            cache_directory.path(),
        )?;
        cached_build.push(started.elapsed());
        cached_parsing.push(timings.parsing);
        cached_graph.push(timings.graph_construction);
        validate_analysis(&repository, &fixture)?;
        validate_cache_counts(timings, file_count, 0, file_count, file_count, 0)?;
        black_box(repository);
    }

    let invalidated = directory.path().join(format!(
        "src/generated/module_{:05}.py",
        fixture.source_modules - 1,
    ));
    let mut incremental_parsing = Vec::with_capacity(samples);
    let mut incremental_graph = Vec::with_capacity(samples);
    let mut incremental_persistence = Vec::with_capacity(samples);
    let mut incremental_build = Vec::with_capacity(samples);
    for revision in 0..samples {
        let mut file = OpenOptions::new().append(true).open(&invalidated)?;
        writeln!(file, "# benchmark revision {revision}")?;
        drop(file);

        let started = Instant::now();
        let (repository, timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
            directory.path(),
            cache_directory.path(),
        )?;
        incremental_build.push(started.elapsed());
        incremental_parsing.push(timings.parsing);
        incremental_graph.push(timings.graph_construction);
        incremental_persistence.push(timings.cache_persistence);
        validate_analysis(&repository, &fixture)?;
        validate_cache_counts(timings, file_count - 1, 1, file_count, file_count - 1, 1)?;
        black_box(repository);
    }

    println!(
        "{} Python files ({} source modules, {} tests; {samples} samples)",
        fixture.python_files, fixture.source_modules, fixture.test_files,
    );
    println!("  {:<20} {:>12} {:>12}", "phase", "median", "minimum");
    print_measurement("discovery", &discovery);
    print_measurement("parsing", &parsing);
    print_measurement("graph construction", &graph_construction);
    print_measurement("complete build", &complete_build);
    print_measurement("impact traversal", &impact);
    print_measurement("cached parsing", &cached_parsing);
    print_measurement("cached graph", &cached_graph);
    print_measurement("cached no-change", &cached_build);
    print_measurement("one-file parsing", &incremental_parsing);
    print_measurement("one-file graph", &incremental_graph);
    print_measurement("cache persistence", &incremental_persistence);
    print_measurement("one-file rebuild", &incremental_build);
    println!();

    Ok(())
}

fn validate_cache_counts(
    timings: AnalysisTimings,
    expected_hits: usize,
    expected_misses: usize,
    expected_module_hits: usize,
    expected_edge_hits: usize,
    expected_edge_misses: usize,
) -> Result<(), Box<dyn Error>> {
    if timings.cache.hits() != expected_hits || timings.cache.misses != expected_misses {
        return Err(format!(
            "cache mismatch: expected {expected_hits} hits/{expected_misses} misses, found {}/{}",
            timings.cache.hits(),
            timings.cache.misses,
        )
        .into());
    }
    if timings.graph_cache.module_hits != expected_module_hits
        || timings.graph_cache.edge_hits != expected_edge_hits
        || timings.graph_cache.edge_misses != expected_edge_misses
    {
        return Err(format!(
            "graph cache mismatch: expected {expected_module_hits} module hits and {expected_edge_hits}/{expected_edge_misses} edge hits/misses, found {} and {}/{}",
            timings.graph_cache.module_hits,
            timings.graph_cache.edge_hits,
            timings.graph_cache.edge_misses,
        )
        .into());
    }
    Ok(())
}

fn record_timings(
    timings: AnalysisTimings,
    discovery: &mut Vec<Duration>,
    parsing: &mut Vec<Duration>,
    graph_construction: &mut Vec<Duration>,
) {
    discovery.push(timings.discovery);
    parsing.push(timings.parsing);
    graph_construction.push(timings.graph_construction);
    black_box(timings.total());
}

fn validate_analysis(
    repository: &RepositoryAnalysis,
    fixture: &SyntheticRepository,
) -> Result<(), Box<dyn Error>> {
    let summary = repository.summary();
    if summary.python_files != fixture.python_files || summary.tests != fixture.test_files {
        return Err(format!(
            "generated repository mismatch: expected {} files/{} tests, indexed {}/{}",
            fixture.python_files, fixture.test_files, summary.python_files, summary.tests,
        )
        .into());
    }
    black_box(summary);
    Ok(())
}

fn validate_impact(
    impact: &urmare_core::ImpactResult,
    fixture: &SyntheticRepository,
) -> Result<(), Box<dyn Error>> {
    let affected = impact.directly_affected.len() + impact.transitively_affected.len();
    let expected_affected = fixture.python_files - 2;
    if affected != expected_affected || impact.affected_tests.len() != fixture.test_files {
        return Err(format!(
            "impact mismatch: expected {expected_affected} affected files/{} tests, found {affected}/{}",
            fixture.test_files,
            impact.affected_tests.len(),
        )
        .into());
    }
    Ok(())
}

fn print_measurement(label: &str, samples: &[Duration]) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let minimum = ordered[0];
    println!(
        "  {label:<20} {:>9.3} ms {:>9.3} ms",
        milliseconds(median),
        milliseconds(minimum),
    );
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
