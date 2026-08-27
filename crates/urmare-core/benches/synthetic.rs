#[path = "../benchmarking/synthetic_repository.rs"]
mod synthetic_repository;

use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use synthetic_repository::{SyntheticRepository, generate};
use tempfile::tempdir;
use urmare_core::{
    AnalysisTimings, ImpactResult, IndexBuildKind, IndexFallbackReason, QueryProfile,
    RepositoryAnalysis,
};

const CASES: &[usize] = &[1_000, 10_000];

fn main() -> Result<(), Box<dyn Error>> {
    println!("Urmare persistent-index synthetic benchmark");
    println!("Repository generation, correctness checks, and fixture mutations are excluded.\n");

    for &file_count in CASES {
        run_case(file_count)?;
    }
    if std::env::var("URMARE_BENCH_50000").is_ok_and(|value| value == "1") {
        run_case(50_000)?;
    } else {
        println!("50,000-file case skipped; set URMARE_BENCH_50000=1 to opt in.");
    }
    Ok(())
}

fn run_case(file_count: usize) -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let cache_directory = tempdir()?;
    let fixture = generate(directory.path(), file_count)?;
    initialize_git(directory.path())?;

    let mut builds = Vec::new();
    let (cold_repository, cold) = measured_build(directory.path(), cache_directory.path())?;
    validate_analysis(&cold_repository, &fixture, file_count)?;
    assert_eq!(cold.timings.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(cold.timings.index_work.files_parsed, file_count);
    assert_eq!(cold.timings.index_work.importers_reresolved, file_count);
    assert!(cold.timings.index_work.index_records_written > file_count);
    assert!(cold.timings.index_work.bytes_written > 0);
    builds.push(("cold full index", cold));

    let (warm, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_analysis(&warm, &fixture, file_count)?;
    assert_no_change_work(&measurement.timings);
    builds.push(("warm no-change", measurement));

    let last_index = fixture.source_modules - 1;
    let last_path = directory.path().join(module_path(last_index));
    let original = fs::read_to_string(&last_path)?;
    fs::write(
        &last_path,
        format!("{original}\n# content-only benchmark edit\n"),
    )?;
    let (content_only, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &content_only,
        &fixture.changed_file,
        &relative_module_path(last_index),
    )?;
    assert_eq!(measurement.timings.index_work.files_parsed, 1);
    assert_eq!(measurement.timings.index_work.importers_reresolved, 0);
    assert_eq!(measurement.timings.index_work.forward_edges_added, 0);
    assert_eq!(measurement.timings.index_work.forward_edges_removed, 0);
    assert!(measurement.timings.index_work.index_records_written <= 2);
    builds.push(("one-file content edit", measurement));

    fs::write(&last_path, format!("{original}\nimport future_candidate\n"))?;
    let (import_edit, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &import_edit,
        &fixture.changed_file,
        &relative_module_path(last_index),
    )?;
    assert_eq!(measurement.timings.index_work.files_parsed, 1);
    assert_eq!(measurement.timings.index_work.importers_reresolved, 1);
    assert_eq!(measurement.timings.index_work.forward_edges_added, 0);
    builds.push(("one-file import edit", measurement));

    let candidate = directory.path().join("src/future_candidate.py");
    fs::write(&candidate, "VALUE = 1\n")?;
    let (module_added, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &module_added,
        &fixture.changed_file,
        &relative_module_path(last_index),
    )?;
    assert_eq!(measurement.timings.index_work.files_parsed, 1);
    assert_eq!(measurement.timings.index_work.modules_added, 1);
    assert_eq!(measurement.timings.index_work.importers_reresolved, 2);
    assert_eq!(measurement.timings.index_work.forward_edges_added, 1);
    builds.push(("candidate module add", measurement));

    fs::remove_file(&candidate)?;
    let (module_deleted, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &module_deleted,
        &fixture.changed_file,
        &relative_module_path(last_index),
    )?;
    assert_eq!(measurement.timings.index_work.files_parsed, 0);
    assert_eq!(measurement.timings.index_work.modules_removed, 1);
    assert_eq!(measurement.timings.index_work.importers_reresolved, 1);
    assert_eq!(measurement.timings.index_work.forward_edges_removed, 1);
    builds.push(("candidate module delete", measurement));

    let renamed_relative =
        PathBuf::from(format!("src/generated/renamed_module_{last_index:05}.py"));
    fs::rename(&last_path, directory.path().join(&renamed_relative))?;
    let (renamed, measurement) = measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &renamed,
        &fixture.changed_file,
        &renamed_relative,
    )?;
    assert_eq!(measurement.timings.index_work.files_parsed, 1);
    assert_eq!(measurement.timings.index_work.modules_added, 1);
    assert_eq!(measurement.timings.index_work.modules_removed, 1);
    builds.push(("module rename", measurement));

    let configuration = directory.path().join("pyproject.toml");
    let mut source = fs::read_to_string(&configuration)?;
    source.push_str("# configuration invalidation benchmark\n");
    fs::write(configuration, source)?;
    let (repository_after_config, measurement) =
        measured_build(directory.path(), cache_directory.path())?;
    validate_against_fresh(
        directory.path(),
        &repository_after_config,
        &fixture.changed_file,
        &renamed_relative,
    )?;
    assert_eq!(
        measurement.timings.index_work.build_kind,
        IndexBuildKind::Full
    );
    assert_eq!(
        measurement.timings.index_work.fallback_reason,
        Some(IndexFallbackReason::ConfigurationChanged)
    );
    assert_eq!(measurement.timings.index_work.files_parsed, file_count);
    builds.push(("configuration rebuild", measurement));
    let repository = repository_after_config;

    let (narrow, narrow_profile) = measured_impact(&repository, &renamed_relative)?;
    validate_impact_against_fresh(directory.path(), &renamed_relative, &narrow)?;
    assert!(narrow.directly_affected.is_empty());
    assert!(narrow_profile.index_records_read <= 4);

    let (broad, broad_profile) = measured_impact(&repository, &fixture.changed_file)?;
    validate_impact(&broad, &fixture)?;
    validate_impact_against_fresh(directory.path(), &fixture.changed_file, &broad)?;
    assert!(broad_profile.index_records_read >= broad.directly_affected.len());

    println!(
        "{} Python files ({} source modules, {} tests)",
        fixture.python_files, fixture.source_modules, fixture.test_files,
    );
    println!(
        "  {:<27} {:>10} {:>9} {:>9} {:>9} {:>8} {:>9} {:>9}",
        "index operation", "total", "delta", "update", "persist", "parses", "resolves", "records"
    );
    for (label, measurement) in &builds {
        print_build(label, measurement);
    }
    println!(
        "  {:<27} {:>10} {:>10} {:>12}",
        "query", "total", "records", "result files"
    );
    print_query("narrow impact", &narrow_profile, impact_size(&narrow));
    print_query("broad impact", &broad_profile, impact_size(&broad));
    println!();

    black_box(repository);
    Ok(())
}

struct BuildMeasurement {
    elapsed: Duration,
    timings: AnalysisTimings,
}

fn measured_build(
    root: &Path,
    cache: &Path,
) -> Result<(RepositoryAnalysis, BuildMeasurement), Box<dyn Error>> {
    let started = Instant::now();
    let (repository, timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root, cache)?;
    Ok((
        repository,
        BuildMeasurement {
            elapsed: started.elapsed(),
            timings,
        },
    ))
}

fn measured_impact(
    repository: &RepositoryAnalysis,
    changed: &Path,
) -> Result<(ImpactResult, QueryProfile), Box<dyn Error>> {
    Ok(repository.impact_profiled(changed)?)
}

fn validate_against_fresh(
    root: &Path,
    incremental: &RepositoryAnalysis,
    broad: &Path,
    narrow: &Path,
) -> Result<(), Box<dyn Error>> {
    let (fresh, _) = RepositoryAnalysis::build_uncached_profiled(root)?;
    if incremental.summary() != fresh.summary()
        || incremental.unresolved_imports()? != fresh.unresolved_imports()?
        || incremental.impact(broad)? != fresh.impact(broad)?
        || incremental.impact(narrow)? != fresh.impact(narrow)?
    {
        return Err("incremental benchmark result differs from a fresh build".into());
    }
    Ok(())
}

fn validate_impact_against_fresh(
    root: &Path,
    changed: &Path,
    incremental: &ImpactResult,
) -> Result<(), Box<dyn Error>> {
    let (fresh, _) = RepositoryAnalysis::build_uncached_profiled(root)?;
    if &fresh.impact(changed)? != incremental {
        return Err("profiled impact differs from a fresh build".into());
    }
    Ok(())
}

fn assert_no_change_work(timings: &AnalysisTimings) {
    assert_eq!(timings.index_work.build_kind, IndexBuildKind::Reused);
    assert_eq!(timings.index_work.files_parsed, 0);
    assert_eq!(timings.index_work.importers_reresolved, 0);
    assert_eq!(timings.index_work.forward_edges_added, 0);
    assert_eq!(timings.index_work.forward_edges_removed, 0);
    assert_eq!(timings.index_work.index_records_written, 0);
    assert_eq!(timings.index_work.bytes_written, 0);
}

fn validate_analysis(
    repository: &RepositoryAnalysis,
    fixture: &SyntheticRepository,
    expected_files: usize,
) -> Result<(), Box<dyn Error>> {
    let summary = repository.summary();
    if summary.python_files != expected_files || summary.tests != fixture.test_files {
        return Err(format!(
            "generated repository mismatch: expected {expected_files} files/{} tests, indexed {}/{}",
            fixture.test_files, summary.python_files, summary.tests,
        )
        .into());
    }
    black_box(summary);
    Ok(())
}

fn validate_impact(
    impact: &ImpactResult,
    fixture: &SyntheticRepository,
) -> Result<(), Box<dyn Error>> {
    let affected = impact_size(impact);
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

fn impact_size(impact: &ImpactResult) -> usize {
    impact.directly_affected.len() + impact.transitively_affected.len()
}

fn print_build(label: &str, measurement: &BuildMeasurement) {
    let timings = &measurement.timings;
    println!(
        "  {label:<27} {:>7.3} ms {:>6.3} ms {:>6.3} ms {:>6.3} ms {:>8} {:>9} {:>9} ({:>8} bytes)",
        milliseconds(measurement.elapsed),
        milliseconds(timings.index_timings.delta_detection),
        milliseconds(timings.index_timings.update),
        milliseconds(timings.index_timings.persistence),
        timings.index_work.files_parsed,
        timings.index_work.importers_reresolved,
        timings.index_work.index_records_written,
        timings.index_work.bytes_written,
    );
}

fn print_query(label: &str, profile: &QueryProfile, results: usize) {
    println!(
        "  {label:<27} {:>7.3} ms {:>10} {:>12}",
        milliseconds(profile.total()),
        profile.index_records_read,
        results,
    );
}

fn relative_module_path(index: usize) -> PathBuf {
    PathBuf::from(module_path(index))
}

fn module_path(index: usize) -> String {
    format!("src/generated/module_{index:05}.py")
}

fn initialize_git(root: &Path) -> Result<(), Box<dyn Error>> {
    git(root, &["init", "--quiet"])?;
    git(root, &["add", "."])?;
    git(
        root,
        &[
            "-c",
            "user.name=Urmare Benchmarks",
            "-c",
            "user.email=urmare@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ],
    )
}

fn git(root: &Path, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
