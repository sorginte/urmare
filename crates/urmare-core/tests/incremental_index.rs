use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use redb::{Database, ReadableTable, TableDefinition};
use tempfile::{TempDir, tempdir};
use urmare_core::{
    AnalysisTimings, DependencyPath, IndexBuildKind, IndexFallbackReason, RepositoryAnalysis,
};

const INDEX_FILE: &str = "repository-index.redb";
const META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("metadata");
const FILES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const REVERSE_TABLE: TableDefinition<&str, u8> = TableDefinition::new("reverse");
const LEGACY_REVERSE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("reverse");

#[test]
fn deterministic_mutation_sequence_matches_a_fresh_build_after_every_step() {
    let fixture = Fixture::new(&[
        (
            "pyproject.toml",
            concat!(
                "[tool.urmare]\n",
                "source-roots = [\"src\"]\n",
                "test-roots = [\"tests\", \"checks\"]\n",
                "exclude = [\"src/excluded/**\"]\n",
            ),
        ),
        ("src/pkg/__init__.py", ""),
        ("src/pkg/base.py", "VALUE = 1\n"),
        ("src/pkg/consumer.py", "from pkg import base\n"),
        ("src/pkg/a.py", "from pkg import b\n"),
        ("src/pkg/b.py", "from pkg import a\n"),
        ("src/ns/feature.py", "FEATURE = True\n"),
        ("src/excluded/ignored.py", "IGNORED = True\n"),
        ("tests/test_consumer.py", "from pkg import consumer\n"),
        ("checks/consumer_spec.py", "from pkg import consumer\n"),
    ]);

    let cold = fixture.assert_parity();
    assert_eq!(cold.index_work.build_kind, IndexBuildKind::Full);

    fixture.write(
        "src/pkg/consumer.py",
        "from pkg import base\n\n# no import change\n",
    );
    let content_only = fixture.assert_parity();
    assert_eq!(content_only.index_work.files_parsed, 1);
    assert_eq!(content_only.index_work.importers_reresolved, 0);
    assert_eq!(content_only.index_work.forward_edges_added, 0);
    assert_eq!(content_only.index_work.forward_edges_removed, 0);
    assert!(content_only.index_work.bytes_written > 0);

    fixture.write(
        "src/pkg/consumer.py",
        "from ns import feature\nimport missing_mod\n",
    );
    let import_edit = fixture.assert_parity();
    assert_eq!(import_edit.index_work.files_parsed, 1);
    assert_eq!(import_edit.index_work.importers_reresolved, 1);
    assert!(import_edit.index_work.forward_edges_added > 0);
    assert!(import_edit.index_work.forward_edges_removed > 0);

    // Several staged, unstaged, and untracked edits occur before one update.
    fixture.write("src/pkg/a.py", "VALUE = 'no cycle'\n");
    fixture.git(&["add", "src/pkg/a.py"]);
    fixture.write("src/pkg/b.py", "from ns import feature\n");
    fixture.write("src/missing_mod.py", "VALUE = 2\n");
    let mixed = fixture.assert_parity();
    assert_eq!(mixed.index_work.files_parsed, 3);
    assert_eq!(mixed.index_work.modules_added, 1);
    assert!(mixed.index_work.importers_reresolved < 9);

    fixture.remove("src/missing_mod.py");
    let deleted = fixture.assert_parity();
    assert_eq!(deleted.index_work.files_parsed, 0);
    assert_eq!(deleted.index_work.modules_removed, 1);
    assert_eq!(deleted.index_work.importers_reresolved, 1);

    fixture.rename("src/pkg/base.py", "src/pkg/moved.py");
    fixture.assert_parity();
    fixture.rename("src/pkg/moved.py", "src/other/moved.py");
    fixture.assert_parity();

    fixture.write("src/ns/__init__.py", "NAMESPACE = True\n");
    fixture.assert_parity();
    fixture.remove("src/ns/__init__.py");
    fixture.assert_parity();

    fixture.write("tests/test_added.py", "from ns import feature\n");
    fixture.assert_parity();
    fixture.rename("tests/test_added.py", "tests/added_helper.py");
    fixture.assert_parity();
    fixture.remove("tests/added_helper.py");
    fixture.assert_parity();

    fixture.commit("mutation sequence");
    let committed = fixture.assert_parity();
    assert_eq!(committed.index_work.files_parsed, 0);
    assert_eq!(committed.index_work.importers_reresolved, 0);
    assert_eq!(committed.index_work.forward_edges_added, 0);
    assert_eq!(committed.index_work.forward_edges_removed, 0);

    let current_branch = fixture.git_output(&["branch", "--show-current"]);
    fixture.git(&["switch", "--quiet", "-c", "older-index-state", "HEAD~1"]);
    fixture.assert_parity();
    fixture.git(&["switch", "--quiet", current_branch.trim()]);
    fixture.assert_parity();

    let current = fixture.read("src/pkg/consumer.py");
    fixture.write("src/pkg/consumer.py", "import restored_dirty_state\n");
    fixture.assert_parity();
    fixture.write("src/pkg/consumer.py", &current);
    fixture.assert_parity();
}

#[test]
fn moves_across_source_roots_and_exclusion_boundaries_are_order_independent() {
    let fixture = Fixture::new(&[
        (
            "pyproject.toml",
            concat!(
                "[tool.urmare]\n",
                "source-roots = [\"z_source\", \"a_source\", \"src\"]\n",
                "exclude = [\"src/generated/**\"]\n",
            ),
        ),
        ("z_source/pkg/item.py", "VALUE = 1\n"),
        ("a_source/.keep", ""),
        ("src/pkg/live.py", "VALUE = 1\n"),
        ("tests/test_items.py", "import pkg.item\nimport pkg.live\n"),
    ]);
    fixture.assert_parity();

    // The destination sorts before the source. All removals must therefore be
    // planned before duplicate-module validation for the addition.
    fixture.rename("z_source/pkg/item.py", "a_source/pkg/item.py");
    let moved = fixture.assert_parity();
    assert_eq!(moved.index_work.modules_added, 1);
    assert_eq!(moved.index_work.modules_removed, 1);
    assert_eq!(moved.index_work.importers_reresolved, 2);

    fixture.rename("src/pkg/live.py", "src/generated/live.py");
    fixture.assert_parity();
    fixture.rename("src/generated/live.py", "src/pkg/live.py");
    fixture.assert_parity();
}

#[test]
fn configuration_add_delete_and_rename_force_complete_rebuilds() {
    let added = Fixture::new(&[
        ("module.py", "VALUE = 1\n"),
        ("test_module.py", "import module\n"),
    ]);
    added.assert_parity();
    added.write("pyproject.toml", "[tool.urmare]\n");
    assert_configuration_rebuild(&added);
    added.remove("pyproject.toml");
    assert_configuration_rebuild(&added);

    let renamed = Fixture::new(&[
        ("pyproject.toml", "[tool.urmare]\n"),
        ("module.py", "VALUE = 1\n"),
    ]);
    renamed.assert_parity();
    renamed.rename("pyproject.toml", "project.toml");
    assert_configuration_rebuild(&renamed);
}

#[test]
fn missing_corrupt_incompatible_and_uncommitted_indexes_recover_safely() {
    let fixture = Fixture::new(&[
        ("module.py", "VALUE = 1\n"),
        ("test_module.py", "import module\n"),
    ]);
    fixture.assert_parity();
    let index = fixture.index_path();

    fs::remove_file(&index).expect("remove index");
    let missing = fixture.assert_parity();
    assert_eq!(
        missing.index_work.fallback_reason,
        Some(IndexFallbackReason::MissingIndex)
    );

    fs::write(&index, b"truncated index").expect("truncate index");
    let corrupt = fixture.assert_parity();
    assert_eq!(
        corrupt.index_work.fallback_reason,
        Some(IndexFallbackReason::IndexCorrupt)
    );

    rewrite_schema_version(&index, 999);
    let incompatible = fixture.assert_parity();
    assert_eq!(
        incompatible.index_work.fallback_reason,
        Some(IndexFallbackReason::IncompatibleIndex)
    );

    rewrite_as_legacy_relationship_schema(&index);
    let migrated = fixture.assert_parity();
    assert_eq!(
        migrated.index_work.fallback_reason,
        Some(IndexFallbackReason::IncompatibleIndex)
    );
    let after_migration = fixture.assert_parity();
    assert_eq!(
        after_migration.index_work.build_kind,
        IndexBuildKind::Reused
    );

    // Dropping a write transaction without commit simulates an interrupted
    // update. Redb must retain the preceding committed generation.
    let database = Database::create(&index).expect("open index");
    {
        let transaction = database.begin_write().expect("write transaction");
        {
            let mut files = transaction.open_table(FILES_TABLE).expect("files table");
            files
                .insert("module.py", b"incomplete".as_slice())
                .expect("tentative write");
        }
        drop(transaction);
    }
    drop(database);
    let recovered = fixture.assert_parity();
    assert_eq!(recovered.index_work.build_kind, IndexBuildKind::Reused);
}

#[test]
fn another_process_holding_the_index_never_blocks_correct_analysis() {
    if std::env::var_os("URMARE_INDEX_LOCK_CHILD").is_some() {
        run_index_lock_child();
        return;
    }

    let fixture = Fixture::new(&[
        ("module.py", "VALUE = 1\n"),
        ("test_module.py", "import module\n"),
    ]);
    fixture.assert_parity();
    let ready = fixture.cache.path().join("child-ready");
    let release = fixture.cache.path().join("child-release");
    let mut child = spawn_index_lock_child(&fixture.index_path(), &ready, &release);
    wait_for_path(&ready, &mut child);

    let locked = fixture.assert_parity();
    assert_eq!(locked.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        locked.index_work.fallback_reason,
        Some(IndexFallbackReason::IndexLocked)
    );

    fs::write(&release, b"release").expect("release child");
    let status = child.wait().expect("wait for child");
    assert!(status.success(), "index lock child failed with {status}");
}

#[test]
fn ignored_python_files_are_not_omitted_from_incremental_detection() {
    let fixture = Fixture::new(&[
        (".gitignore", "ignored.py\n"),
        ("module.py", "VALUE = 1\n"),
        ("ignored.py", "IGNORED = 1\n"),
    ]);
    let cold = fixture.assert_parity();
    assert_eq!(cold.index_work.files_parsed, 2);

    fixture.write("ignored.py", "IGNORED = 2\n");
    let modified = fixture.assert_parity();
    assert_eq!(modified.index_work.files_parsed, 1);
    assert_eq!(modified.index_work.modules_reused, 1);

    fixture.remove("ignored.py");
    let removed = fixture.assert_parity();
    assert_eq!(removed.index_work.modules_removed, 1);
}

#[test]
fn timestamps_and_same_size_content_changes_preserve_hash_correctness() {
    let fixture = Fixture::new(&[("module.py", "VALUE = 1\n")]);
    fixture.assert_parity();
    let path = fixture.repository.path().join("module.py");
    let original_modified = fs::metadata(&path)
        .expect("module metadata")
        .modified()
        .unwrap_or(SystemTime::now());

    let file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open module");
    file.set_times(fs::FileTimes::new().set_modified(original_modified + Duration::from_secs(2)))
        .expect("change timestamp");
    let timestamp_only = fixture.assert_parity();
    assert_eq!(timestamp_only.index_work.build_kind, IndexBuildKind::Reused);
    assert_eq!(timestamp_only.index_work.files_parsed, 0);

    fixture.write("module.py", "VALUE = 2\n");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open changed module");
    file.set_times(fs::FileTimes::new().set_modified(original_modified))
        .expect("restore timestamp");
    let same_size_and_time = fixture.assert_parity();
    assert_eq!(same_size_and_time.index_work.files_hashed, 1);
    assert_eq!(same_size_and_time.index_work.files_parsed, 1);
}

#[test]
fn non_git_repositories_and_duplicate_modules_use_correct_full_fallbacks() {
    let repository = tempdir().expect("non-Git repository");
    let cache = tempdir().expect("non-Git cache");
    write_file(repository.path(), "module.py", "VALUE = 1\n");
    RepositoryAnalysis::build_profiled_with_cache_directory(repository.path(), cache.path())
        .expect("initial non-Git analysis");
    let (fallback, timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(repository.path(), cache.path())
            .expect("non-Git full fallback");
    assert_eq!(timings.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        timings.index_work.fallback_reason,
        Some(IndexFallbackReason::NonGitRepository)
    );
    assert_eq!(timings.index_work.files_parsed, 1);
    let fresh = RepositoryAnalysis::build_uncached_profiled(repository.path())
        .expect("fresh non-Git analysis")
        .0;
    assert_eq!(
        fallback.graph_inspection(None).expect("fallback graph"),
        fresh.graph_inspection(None).expect("fresh graph")
    );

    let duplicate = Fixture::new(&[
        (
            "pyproject.toml",
            "[tool.urmare]\nsource-roots = [\"a\", \"b\"]\n",
        ),
        ("a/pkg/item.py", "VALUE = 1\n"),
        ("b/.keep", ""),
    ]);
    duplicate.assert_parity();
    duplicate.write("b/pkg/item.py", "VALUE = 2\n");
    let incremental_error = RepositoryAnalysis::build_profiled_with_cache_directory(
        duplicate.repository.path(),
        duplicate.cache.path(),
    )
    .err()
    .expect("incremental duplicate-module error");
    let fresh_error = RepositoryAnalysis::build_uncached_profiled(duplicate.repository.path())
        .err()
        .expect("fresh duplicate-module error");
    assert_eq!(incremental_error.to_string(), fresh_error.to_string());
    duplicate.remove("b/pkg/item.py");
    duplicate.assert_parity();
}

#[test]
fn git_index_flags_that_hide_content_changes_force_a_complete_fallback() {
    let fixture = Fixture::new(&[("module.py", "VALUE = 1\n")]);
    fixture.assert_parity();
    fixture.git(&["update-index", "--assume-unchanged", "module.py"]);
    fixture.write("module.py", "VALUE = 2\n");

    let hidden_change = fixture.assert_parity();
    assert_eq!(hidden_change.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        hidden_change.index_work.fallback_reason,
        Some(IndexFallbackReason::GitStateUnavailable)
    );
    assert_eq!(hidden_change.index_work.files_parsed, 1);
}

#[test]
fn skip_worktree_files_force_a_complete_fallback() {
    let fixture = Fixture::new(&[("module.py", "VALUE = 1\n")]);
    fixture.assert_parity();
    fixture.git(&["update-index", "--skip-worktree", "module.py"]);

    let hidden_path = fixture.assert_parity();
    assert_eq!(hidden_path.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        hidden_path.index_work.fallback_reason,
        Some(IndexFallbackReason::GitStateUnavailable)
    );
    assert_eq!(hidden_path.index_work.files_parsed, 1);
}

#[test]
fn gitmodules_file_forces_a_complete_fallback() {
    let fixture = Fixture::new(&[("module.py", "VALUE = 1\n")]);
    fixture.assert_parity();
    fixture.write(
        ".gitmodules",
        "[submodule \"nested\"]\npath = nested\nurl = ../nested\n",
    );

    let submodule_boundary = fixture.assert_parity();
    assert_eq!(
        submodule_boundary.index_work.build_kind,
        IndexBuildKind::Full
    );
    assert_eq!(
        submodule_boundary.index_work.fallback_reason,
        Some(IndexFallbackReason::GitStateUnavailable)
    );
    assert_eq!(submodule_boundary.index_work.files_parsed, 1);
}

#[test]
fn nested_untracked_repository_forces_a_complete_fallback() {
    let fixture = Fixture::new(&[("module.py", "VALUE = 1\n")]);
    fixture.assert_parity();
    fixture.write("nested/nested_module.py", "NESTED = True\n");
    git(
        &fixture.repository.path().join("nested"),
        &["init", "--quiet"],
    );

    let nested_repository = fixture.assert_parity();
    assert_eq!(
        nested_repository.index_work.build_kind,
        IndexBuildKind::Full
    );
    assert_eq!(
        nested_repository.index_work.fallback_reason,
        Some(IndexFallbackReason::GitStateUnavailable)
    );
    assert_eq!(nested_repository.index_work.files_parsed, 2);
}

#[test]
fn default_ignored_directories_do_not_expand_the_warm_delta_inventory() {
    let fixture = Fixture::new(&[(".gitignore", ".venv/\n"), ("module.py", "VALUE = 1\n")]);
    for index in 0..32 {
        fixture.write(
            &format!(".venv/lib/python/site-packages/package_{index}.py"),
            "IGNORED = True\n",
        );
    }

    let cold = fixture.assert_parity();
    assert_eq!(cold.index_work.files_parsed, 1);
    let warm = fixture.assert_parity();
    assert_eq!(warm.index_work.build_kind, IndexBuildKind::Reused);
    assert_eq!(warm.index_work.inventory_entries_inspected, 1);
    assert_eq!(warm.index_work.files_statted, 0);
    assert_eq!(warm.index_work.files_parsed, 0);
}

#[test]
fn query_time_storage_failure_uses_a_correct_uncached_view() {
    let fixture = Fixture::new(&[
        ("module.py", "VALUE = 1\n"),
        ("consumer.py", "import module\n"),
        ("test_consumer.py", "import consumer\n"),
    ]);
    let (repository, _) = RepositoryAnalysis::build_profiled_with_cache_directory(
        fixture.repository.path(),
        fixture.cache.path(),
    )
    .expect("persistent analysis");
    let fresh = RepositoryAnalysis::build_uncached_profiled(fixture.repository.path())
        .expect("fresh analysis")
        .0;
    fs::write(fixture.index_path(), b"broken after build").expect("break stored index");

    let (impact, profile) = repository
        .impact_profiled(Path::new("module.py"))
        .expect("profiled recovery impact");
    assert_eq!(
        impact,
        fresh.impact(Path::new("module.py")).expect("fresh impact")
    );
    assert!(profile.fallback_rebuild > Duration::ZERO);
    assert_eq!(
        repository
            .why(Path::new("module.py"), Path::new("test_consumer.py"))
            .expect("recovery explanation"),
        fresh
            .why(Path::new("module.py"), Path::new("test_consumer.py"))
            .expect("fresh explanation")
    );
    assert_eq!(
        repository
            .graph_inspection(None)
            .expect("recovery graph inspection"),
        fresh
            .graph_inspection(None)
            .expect("fresh graph inspection")
    );
}

fn assert_configuration_rebuild(fixture: &Fixture) {
    let timings = fixture.assert_parity();
    assert_eq!(timings.index_work.build_kind, IndexBuildKind::Full);
    assert_eq!(
        timings.index_work.fallback_reason,
        Some(IndexFallbackReason::ConfigurationChanged)
    );
    assert_eq!(timings.index_work.files_parsed, fixture.python_file_count());
}

fn rewrite_schema_version(index: &Path, version: u64) {
    let database = Database::create(index).expect("open index");
    let transaction = database.begin_write().expect("write transaction");
    {
        let mut table = transaction.open_table(META_TABLE).expect("metadata table");
        let bytes = table
            .get("current")
            .expect("metadata lookup")
            .expect("metadata record")
            .value()
            .to_vec();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&bytes).expect("metadata JSON");
        metadata["schema_version"] = version.into();
        let bytes = serde_json::to_vec(&metadata).expect("updated metadata JSON");
        table
            .insert("current", bytes.as_slice())
            .expect("replace metadata");
    }
    transaction.commit().expect("commit incompatible metadata");
}

fn rewrite_as_legacy_relationship_schema(index: &Path) {
    let database = Database::create(index).expect("open index");
    let transaction = database.begin_write().expect("write transaction");
    transaction
        .delete_table(REVERSE_TABLE)
        .expect("remove current relationship table");
    {
        let mut table = transaction
            .open_table(LEGACY_REVERSE_TABLE)
            .expect("legacy relationship table");
        table
            .insert("module.py", b"[]".as_slice())
            .expect("legacy relationship value");
    }
    {
        let mut table = transaction.open_table(META_TABLE).expect("metadata table");
        let bytes = table
            .get("current")
            .expect("metadata lookup")
            .expect("metadata record")
            .value()
            .to_vec();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&bytes).expect("metadata JSON");
        metadata["schema_version"] = 1.into();
        let bytes = serde_json::to_vec(&metadata).expect("legacy metadata JSON");
        table
            .insert("current", bytes.as_slice())
            .expect("replace metadata");
    }
    transaction.commit().expect("commit legacy index shape");
}

fn spawn_index_lock_child(index: &Path, ready: &Path, release: &Path) -> Child {
    Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("another_process_holding_the_index_never_blocks_correct_analysis")
        .arg("--nocapture")
        .env("URMARE_INDEX_LOCK_CHILD", "1")
        .env("URMARE_INDEX_PATH", index)
        .env("URMARE_INDEX_READY", ready)
        .env("URMARE_INDEX_RELEASE", release)
        .spawn()
        .expect("spawn index lock child")
}

fn run_index_lock_child() {
    let index = PathBuf::from(std::env::var_os("URMARE_INDEX_PATH").expect("index path"));
    let ready = PathBuf::from(std::env::var_os("URMARE_INDEX_READY").expect("ready path"));
    let release = PathBuf::from(std::env::var_os("URMARE_INDEX_RELEASE").expect("release path"));
    let _database = Database::create(index).expect("child opens index");
    fs::write(ready, b"ready").expect("signal ready");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !release.exists() {
        assert!(
            Instant::now() < deadline,
            "parent did not release index lock"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_path(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child.try_wait().expect("poll child") {
            panic!("index lock child exited early with {status}");
        }
        assert!(Instant::now() < deadline, "timed out waiting for child");
        thread::sleep(Duration::from_millis(10));
    }
}

struct Fixture {
    repository: TempDir,
    cache: TempDir,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Self {
        let repository = tempdir().expect("temporary repository");
        let cache = tempdir().expect("temporary cache");
        for (path, contents) in files {
            write_file(repository.path(), path, contents);
        }
        git(repository.path(), &["init", "--quiet"]);
        git(repository.path(), &["add", "."]);
        commit(repository.path(), "baseline");
        Self { repository, cache }
    }

    fn assert_parity(&self) -> AnalysisTimings {
        let (incremental, timings) = RepositoryAnalysis::build_profiled_with_cache_directory(
            self.repository.path(),
            self.cache.path(),
        )
        .expect("incremental repository analysis");
        let (fresh, _) = RepositoryAnalysis::build_uncached_profiled(self.repository.path())
            .expect("fresh repository analysis");

        assert_eq!(incremental.summary(), fresh.summary());
        let incremental_graph = incremental
            .graph_inspection(None)
            .expect("incremental graph inspection");
        let fresh_graph = fresh
            .graph_inspection(None)
            .expect("fresh graph inspection");
        assert_eq!(incremental_graph, fresh_graph);
        assert_eq!(
            incremental
                .unresolved_imports()
                .expect("incremental unresolved imports"),
            fresh
                .unresolved_imports()
                .expect("fresh unresolved imports")
        );

        let paths: Vec<_> = incremental_graph
            .modules
            .iter()
            .map(|module| module.path.clone())
            .collect();
        for changed in &paths {
            assert_eq!(
                incremental.impact(changed).expect("incremental impact"),
                fresh.impact(changed).expect("fresh impact")
            );
            assert_eq!(
                incremental
                    .affected_tests(changed)
                    .expect("incremental affected tests"),
                fresh.affected_tests(changed).expect("fresh affected tests")
            );
            for affected in &paths {
                assert_eq!(
                    why_outcome(&incremental, changed, affected),
                    why_outcome(&fresh, changed, affected),
                    "why parity for changed={changed:?}, affected={affected:?}",
                );
            }
        }
        timings
    }

    fn write(&self, path: &str, contents: &str) {
        write_file(self.repository.path(), path, contents);
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.repository.path().join(path)).expect("read fixture file")
    }

    fn remove(&self, path: &str) {
        fs::remove_file(self.repository.path().join(path)).expect("remove fixture file");
    }

    fn rename(&self, from: &str, to: &str) {
        let destination = self.repository.path().join(to);
        fs::create_dir_all(destination.parent().expect("destination parent"))
            .expect("create destination parent");
        fs::rename(self.repository.path().join(from), destination).expect("rename fixture file");
    }

    fn git(&self, arguments: &[&str]) {
        git(self.repository.path(), arguments);
    }

    fn git_output(&self, arguments: &[&str]) -> String {
        git_output(self.repository.path(), arguments)
    }

    fn commit(&self, message: &str) {
        git(self.repository.path(), &["add", "-A"]);
        commit(self.repository.path(), message);
    }

    fn index_path(&self) -> PathBuf {
        self.cache.path().join(INDEX_FILE)
    }

    fn python_file_count(&self) -> usize {
        RepositoryAnalysis::build_uncached_profiled(self.repository.path())
            .expect("fresh analysis")
            .0
            .summary()
            .python_files
    }
}

fn why_outcome(
    repository: &RepositoryAnalysis,
    changed: &Path,
    affected: &Path,
) -> Result<DependencyPath, String> {
    repository
        .why(changed, affected)
        .map_err(|error| error.to_string())
}

fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, contents).expect("write fixture file");
}

fn commit(root: &Path, message: &str) {
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
            message,
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

fn git_output(root: &Path, arguments: &[&str]) -> String {
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
    String::from_utf8(output.stdout).expect("Git UTF-8 output")
}
