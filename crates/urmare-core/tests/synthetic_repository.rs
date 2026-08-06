#[path = "../benchmarking/synthetic_repository.rs"]
mod synthetic_repository;

use tempfile::tempdir;
use urmare_core::RepositoryAnalysis;

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
fn parsed_import_cache_reuses_unchanged_files_and_invalidates_one_change() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    let fixture = synthetic_repository::generate(root.path(), 100).expect("synthetic repository");

    let (first, first_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold cached build");
    assert_eq!(first_timings.cache.hits(), 0);
    assert_eq!(first_timings.cache.misses, 100);
    assert_eq!(first_timings.graph_cache.module_hits, 0);
    assert_eq!(first_timings.graph_cache.edge_hits, 0);
    assert_eq!(first_timings.graph_cache.edge_misses, 100);

    let (second, second_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm cached build");
    assert_eq!(second_timings.cache.hits(), 100);
    assert_eq!(second_timings.cache.misses, 0);
    assert_eq!(second_timings.graph_cache.module_hits, 100);
    assert_eq!(second_timings.graph_cache.edge_hits, 100);
    assert_eq!(second_timings.graph_cache.edge_misses, 0);
    assert_eq!(first.summary(), second.summary());

    let changed = root.path().join("src/generated/module_00042.py");
    let mut source = std::fs::read_to_string(&changed).expect("changed source");
    source.push_str("\n# invalidated\n");
    std::fs::write(&changed, source).expect("modify one source file");

    let (third, third_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("incremental cached build");
    assert_eq!(third_timings.cache.hits(), 99);
    assert_eq!(third_timings.cache.misses, 1);
    assert_eq!(third_timings.graph_cache.module_hits, 100);
    assert_eq!(third_timings.graph_cache.edge_hits, 99);
    assert_eq!(third_timings.graph_cache.edge_misses, 1);
    assert_eq!(second.summary(), third.summary());
    assert_eq!(
        third
            .impact(&fixture.changed_file)
            .expect("cached impact")
            .affected_tests
            .len(),
        fixture.test_files
    );
}

#[test]
fn parsed_import_cache_invalidates_when_module_configuration_changes() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    synthetic_repository::generate(root.path(), 20).expect("synthetic repository");

    RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
        .expect("cold cached build");
    let (_, warm) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm cached build");
    assert_eq!(warm.cache.hits(), 20);

    std::fs::write(
        root.path().join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"src\", \".\"]\n",
    )
    .expect("configuration change");
    let (_, invalidated) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("configuration-invalidated build");
    assert_eq!(invalidated.cache.hits(), 0);
    assert_eq!(invalidated.cache.misses, 20);
    assert_eq!(invalidated.graph_cache.module_hits, 0);
    assert_eq!(invalidated.graph_cache.edge_hits, 0);
    assert_eq!(invalidated.graph_cache.edge_misses, 20);
}

#[test]
fn warm_caches_preserve_unresolved_import_locations() {
    let root = tempdir().expect("temporary repository");
    let cache = tempdir().expect("temporary cache");
    std::fs::write(
        root.path().join("module.py"),
        "\nfrom external.api import Client\n",
    )
    .expect("Python fixture");

    let (cold, cold_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold cached build");
    let (warm, warm_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm cached build");

    assert_eq!(cold_timings.cache.misses, 1);
    assert_eq!(warm_timings.cache.hits(), 1);
    assert_eq!(warm_timings.graph_cache.edge_hits, 1);
    assert_eq!(cold.unresolved_imports(), warm.unresolved_imports());
    let [unresolved] = warm.unresolved_imports() else {
        panic!("expected one unresolved import");
    };
    assert_eq!(unresolved.importer, std::path::PathBuf::from("module.py"));
    assert_eq!(unresolved.location.line, 2);
    assert_eq!(unresolved.location.column, 26);
    assert_eq!(
        unresolved.import.to_string(),
        "from external.api import Client"
    );
}

#[test]
fn graph_cache_invalidates_all_edges_when_the_local_module_set_changes() {
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

    let (cold, _) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("cold cached build");
    assert_eq!(cold.summary().unresolved_imports, 1);

    let (_, warm) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("warm cached build");
    assert_eq!(warm.graph_cache.edge_hits, 2);

    std::fs::write(root.path().join("src/candidate.py"), "VALUE = 1\n")
        .expect("newly local module");
    let (added, added_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("module-added build");
    assert_eq!(added_timings.cache.hits(), 2);
    assert_eq!(added_timings.cache.misses, 1);
    assert_eq!(added_timings.graph_cache.module_hits, 2);
    assert_eq!(added_timings.graph_cache.edge_hits, 0);
    assert_eq!(added_timings.graph_cache.edge_misses, 3);
    assert_eq!(added.summary().unresolved_imports, 0);
    let impact = added
        .impact(std::path::Path::new("src/candidate.py"))
        .expect("new local module impact");
    assert_eq!(
        impact.directly_affected,
        [std::path::PathBuf::from("src/consumer.py")]
    );
    assert_eq!(
        impact.affected_tests,
        [std::path::PathBuf::from("tests/test_consumer.py")]
    );

    std::fs::remove_file(root.path().join("src/candidate.py")).expect("remove local module");
    let (removed, removed_timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("module-removed build");
    assert_eq!(removed_timings.graph_cache.module_hits, 2);
    assert_eq!(removed_timings.graph_cache.edge_hits, 0);
    assert_eq!(removed_timings.graph_cache.edge_misses, 2);
    assert_eq!(removed.summary().unresolved_imports, 1);
}
