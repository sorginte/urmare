#[path = "../benchmarking/synthetic_repository.rs"]
mod synthetic_repository;

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::tempdir;
use urmare_core::{IndexBuildKind, IndexFallbackReason, RepositoryAnalysis};

#[test]
fn generated_repository_has_the_requested_scale_and_impact_shape() {
    let root = tempdir().expect("temporary repository");
    let fixture = synthetic_repository::generate(root.path(), 100).expect("synthetic repository");
    let repository = RepositoryAnalysis::build(root.path()).expect("repository analysis");
    let summary = repository.summary();
    assert_eq!(summary.python_files, 100);
    assert_eq!(summary.tests, 10);
    assert_eq!(summary.unresolved_imports, 0);

    let impact = repository
        .impact(&fixture.changed_file)
        .expect("impact analysis");
    assert_eq!(
        impact.directly_affected.len() + impact.transitively_affected.len(),
        98
    );
    assert_eq!(impact.affected_tests.len(), 10);
}

#[test]
fn generator_refuses_to_overwrite_an_existing_destination() {
    let root = tempdir().expect("temporary repository");
    std::fs::write(root.path().join("keep.txt"), "keep me\n").expect("existing file");

    let error = synthetic_repository::generate(root.path(), 100)
        .expect_err("non-empty destination must be rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(root.path().join("keep.txt")).expect("existing file survives"),
        "keep me\n"
    );
}

#[test]
fn clean_index_reuse_and_non_import_edit_have_bounded_work() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    let fixture = synthetic_repository::generate(root.path(), 100).expect("synthetic repository");
    initialize_git(root.path());

    let (first, first_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold index build");
    assert_eq!(first_timings.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(first_timings.index_work.files_parsed, 100);
    assert_eq!(first_timings.index_work.importers_reresolved, 100);

    let (second, second_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm index reuse");
    assert_eq!(second_timings.index_work.build_kind, IndexBuildKind::Reused);
    assert_eq!(second_timings.index_work.files_parsed, 0);
    assert_eq!(second_timings.index_work.importers_reresolved, 0);
    assert_eq!(second_timings.index_work.forward_edges_added, 0);
    assert_eq!(second_timings.index_work.forward_edges_removed, 0);
    assert_eq!(second_timings.index_work.index_records_written, 0);
    assert_eq!(first.summary(), second.summary());

    let narrow = PathBuf::from(format!(
        "src/generated/module_{:05}.py",
        fixture.source_modules - 1
    ));
    let (narrow_impact, narrow_profile) = second
        .impact_profiled(&narrow)
        .expect("profiled narrow impact");
    assert!(narrow_impact.directly_affected.is_empty());
    assert!(narrow_profile.index_records_read <= 4);
    let (explanation, why_profile) = second
        .why_profiled(&fixture.changed_file, &narrow)
        .expect("profiled explanation");
    assert_eq!(explanation.path.len(), fixture.source_modules);
    assert!(why_profile.index_records_read <= fixture.source_modules * 2 + 2);

    let changed = root.path().join("src/generated/module_00042.py");
    let mut source = std::fs::read_to_string(&changed).expect("changed source");
    source.push_str("\n# content-only edit\n");
    std::fs::write(&changed, source).expect("modify one source file");

    let (third, third_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("incremental update");
    assert_eq!(
        third_timings.index_work.build_kind,
        IndexBuildKind::Incremental
    );
    assert_eq!(third_timings.index_work.files_read, 1);
    assert_eq!(third_timings.index_work.files_hashed, 1);
    assert_eq!(third_timings.index_work.files_parsed, 1);
    assert_eq!(third_timings.index_work.importers_reresolved, 0);
    assert_eq!(third_timings.index_work.forward_edges_added, 0);
    assert_eq!(third_timings.index_work.forward_edges_removed, 0);
    assert_eq!(second.summary(), third.summary());
    assert_eq!(
        third
            .impact(&fixture.changed_file)
            .expect("incremental impact")
            .affected_tests
            .len(),
        fixture.test_files
    );
}

#[test]
fn configuration_change_forces_an_explicit_full_rebuild() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    synthetic_repository::generate(root.path(), 20).expect("synthetic repository");
    initialize_git(root.path());

    RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
        .expect("cold index build");
    let (_, warm) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm reuse");
    assert_eq!(warm.index_work.build_kind, IndexBuildKind::Reused);

    std::fs::write(
        root.path().join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"src\", \".\"]\n",
    )
    .expect("configuration change");
    let (_, invalidated) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("configuration-invalidated build");
    assert_eq!(invalidated.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        invalidated.index_work.fallback_reason,
        Some(IndexFallbackReason::ConfigurationChanged)
    );
    assert_eq!(invalidated.index_work.files_parsed, 20);
}

#[test]
fn warm_index_preserves_unresolved_import_locations() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    std::fs::write(
        root.path().join("module.py"),
        "\nfrom external.api import Client\n",
    )
    .expect("Python fixture");
    initialize_git(root.path());

    let (cold, cold_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold index build");
    let (warm, warm_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm index reuse");

    assert_eq!(cold_timings.index_work.files_parsed, 1);
    assert_eq!(warm_timings.index_work.files_parsed, 0);
    assert_eq!(warm_timings.index_work.importers_reresolved, 0);
    assert_eq!(
        cold.unresolved_imports().expect("cold unresolved imports"),
        warm.unresolved_imports().expect("warm unresolved imports")
    );
    let unresolved = warm.unresolved_imports().expect("unresolved imports");
    let [unresolved] = unresolved.as_slice() else {
        panic!("expected one unresolved import");
    };
    assert_eq!(unresolved.importer, PathBuf::from("module.py"));
    assert_eq!(unresolved.location.line, 2);
    assert_eq!(unresolved.location.column, 26);
}

#[test]
fn module_changes_reresolve_only_candidate_dependent_importers() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::create_dir_all(root.path().join("tests")).expect("test directory");
    std::fs::write(root.path().join("src/consumer.py"), "import candidate\n").expect("consumer");
    std::fs::write(
        root.path().join("tests/test_consumer.py"),
        "import consumer\n",
    )
    .expect("test");
    initialize_git(root.path());

    let (cold, _) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold index build");
    assert_eq!(cold.summary().unresolved_imports, 1);

    std::fs::write(root.path().join("src/candidate.py"), "VALUE = 1\n").expect("new local module");
    let (added, added_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("module-added update");
    assert_eq!(added_timings.index_work.files_parsed, 1);
    assert_eq!(added_timings.index_work.modules_added, 1);
    assert_eq!(added_timings.index_work.importers_reresolved, 2);
    assert_eq!(added_timings.index_work.forward_edges_added, 1);
    assert_eq!(added.summary().unresolved_imports, 0);
    assert_eq!(
        added
            .impact(Path::new("src/candidate.py"))
            .expect("new local module impact")
            .affected_tests,
        [PathBuf::from("tests/test_consumer.py")]
    );

    std::fs::remove_file(root.path().join("src/candidate.py")).expect("remove local module");
    let (removed, removed_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("module-removed update");
    assert_eq!(removed_timings.index_work.files_parsed, 0);
    assert_eq!(removed_timings.index_work.modules_removed, 1);
    assert_eq!(removed_timings.index_work.importers_reresolved, 1);
    assert_eq!(removed_timings.index_work.forward_edges_removed, 1);
    assert_eq!(removed.summary().unresolved_imports, 1);
}

fn initialize_git(root: &Path) {
    git(root, &["init", "--quiet"]);
    git(root, &["add", "."]);
    git(
        root,
        &[
            "-c",
            "user.name=Urmare Tests",
            "-c",
            "user.email=urmare@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ],
    );
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("Git is available for tests");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
