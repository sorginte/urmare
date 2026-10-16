use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use urmare_core::{DependencyEvidenceKind, RepositoryAnalysis, StaticImport};

fn write(root: &Path, path: &str, source: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("fixture directory");
    }
    fs::write(path, source).expect("fixture source");
}

#[test]
fn pytest_visibility_and_reexports_propagate_explainable_impact() {
    let root = tempdir().expect("repository");
    write(
        root.path(),
        "src/pkg/__init__.py",
        "from .service import Service as PublicService\n__all__ = [\"PublicService\"]\n",
    );
    write(
        root.path(),
        "src/pkg/service.py",
        "class Service: pass\n__all__ = [\"Service\"]\n",
    );
    write(
        root.path(),
        "src/consumer.py",
        "from pkg import PublicService\n",
    );
    write(
        root.path(),
        "conftest.py",
        "import consumer\nimport pytest\n@pytest.fixture(autouse=True)\ndef database():\n    return 1\n",
    );
    write(
        root.path(),
        "tests/test_consumer.py",
        "from consumer import PublicService\ndef test_service(database):\n    assert PublicService\n",
    );

    let repository = RepositoryAnalysis::build_uncached_profiled(root.path())
        .expect("analysis")
        .0;
    let impact = repository
        .impact(Path::new("src/pkg/service.py"))
        .expect("service impact");
    assert!(
        impact
            .directly_affected
            .iter()
            .any(|path| path == Path::new("src/pkg/__init__.py"))
    );
    assert!(
        impact
            .directly_affected
            .iter()
            .any(|path| path == Path::new("src/consumer.py"))
    );
    assert!(
        impact
            .affected_tests
            .iter()
            .any(|path| path == Path::new("tests/test_consumer.py"))
    );

    let explanation = repository
        .why(
            Path::new("src/pkg/service.py"),
            Path::new("tests/test_consumer.py"),
        )
        .expect("re-export explanation");
    assert!(
        explanation
            .steps
            .iter()
            .flat_map(|step| &step.evidence)
            .any(|e| e.kind == DependencyEvidenceKind::ReExport)
    );

    let config_impact = repository
        .impact(Path::new("conftest.py"))
        .expect("conftest impact");
    assert!(
        config_impact
            .affected_tests
            .iter()
            .any(|path| path == Path::new("tests/test_consumer.py"))
    );
}

#[test]
fn dynamic_import_literals_resolve_and_computed_targets_are_conservative() {
    let root = tempdir().expect("repository");
    write(root.path(), "src/pkg/__init__.py", "");
    write(root.path(), "src/pkg/service.py", "VALUE = 1\n");
    write(
        root.path(),
        "src/loader.py",
        "import importlib\nimportlib.import_module(\"pkg.service\")\n",
    );
    write(
        root.path(),
        "src/computed.py",
        "import importlib\nname = \"pkg.service\"\nimportlib.import_module(name)\n",
    );
    write(
        root.path(),
        "tests/test_service.py",
        "from loader import VALUE\n",
    );

    let repository = RepositoryAnalysis::build_uncached_profiled(root.path())
        .expect("analysis")
        .0;
    let impact = repository
        .impact(Path::new("src/pkg/service.py"))
        .expect("dynamic impact");
    assert!(
        impact
            .directly_affected
            .iter()
            .any(|path| path == Path::new("src/loader.py"))
    );
    assert_eq!(impact.full_validation, None);
    let inspection = repository
        .graph_inspection(Some(Path::new("src/loader.py")))
        .expect("inspection");
    assert!(
        inspection
            .edges
            .iter()
            .flat_map(|edge| &edge.evidence)
            .any(|e| e.kind == DependencyEvidenceKind::DynamicImport)
    );

    let computed = repository
        .impact(Path::new("src/computed.py"))
        .expect("computed dynamic impact");
    assert!(computed.full_validation.is_some());
    let traces = repository
        .graph_inspection(Some(Path::new("src/computed.py")))
        .expect("computed trace");
    assert!(
        traces
            .resolution_traces
            .iter()
            .any(|trace| matches!(trace.import, StaticImport::Dynamic { module: None, .. }))
    );
}

#[test]
fn fixture_semantic_edits_refresh_the_persistent_index_without_reparsing_every_file() {
    let root = tempdir().expect("repository");
    let cache = tempdir().expect("cache");
    write(root.path(), "src/module.py", "VALUE = 1\n");
    write(
        root.path(),
        "conftest.py",
        "import pytest\n@pytest.fixture(name=\"db\")\ndef database():\n    return 1\n",
    );
    write(
        root.path(),
        "tests/test_module.py",
        "def test_value(db):\n    assert True\n",
    );
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["add", "."]);
    git(
        root.path(),
        &[
            "-c",
            "user.name=Urmare Tests",
            "-c",
            "user.email=urmare@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );
    RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
        .expect("cold analysis");

    write(
        root.path(),
        "conftest.py",
        "import pytest\n@pytest.fixture(name=\"database\")\ndef database():\n    return 1\n",
    );
    let (incremental, timings) =
        RepositoryAnalysis::build_profiled_with_cache_directory(root.path(), cache.path())
            .expect("incremental fixture analysis");
    assert_eq!(timings.index_work.files_parsed, 1);
    assert_eq!(
        timings.index_work.build_kind,
        urmare_core::IndexBuildKind::Incremental
    );
    let fresh = RepositoryAnalysis::build_uncached_profiled(root.path())
        .expect("fresh fixture analysis")
        .0;
    assert_eq!(
        incremental
            .impact(Path::new("conftest.py"))
            .expect("incremental impact"),
        fresh
            .impact(Path::new("conftest.py"))
            .expect("fresh impact")
    );
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
