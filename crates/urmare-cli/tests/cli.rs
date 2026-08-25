use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/python-projects")
        .join(name)
}

fn urmare() -> Command {
    Command::cargo_bin("urmare").expect("Urmare test binary")
}

#[test]
fn top_level_help_points_to_command_specific_options() {
    urmare()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Run 'urmare help <COMMAND>' for command-specific options.",
        ));
}

#[test]
fn command_help_documents_options_constraints_and_examples() {
    urmare()
        .args(["graph", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--debug"))
        .stdout(predicate::str::contains("incompatible with --json"))
        .stdout(predicate::str::contains("requires --debug"))
        .stdout(predicate::str::contains("urmare graph --debug"));

    urmare()
        .args(["impact", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "<FILE|--changed|--git-diff <BASE>>",
        ))
        .stdout(predicate::str::contains("urmare impact --changed --json"))
        .stdout(predicate::str::contains(
            "incompatible with --changed and --git-diff",
        ))
        .stdout(predicate::str::contains(
            "urmare impact --git-diff main --json",
        ));

    urmare()
        .args(["tests", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains(
            "urmare tests --affected --git-diff main --json",
        ));

    urmare()
        .args(["why", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains(
            "urmare why src/payments/service.py tests/test_service.py --json",
        ));
}

#[test]
fn version_reports_the_package_version() {
    urmare()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::eq(format!(
            "urmare {}\n",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn impact_requires_exactly_one_change_source() {
    urmare()
        .arg("impact")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "<FILE|--changed|--git-diff <BASE>>",
        ))
        .stderr(predicate::str::contains(
            "Usage: urmare impact <FILE|--changed|--git-diff <BASE>>",
        ))
        .stderr(predicate::str::contains("--git-diff <BASE> <FILE>").not());
}

#[test]
fn graph_summarizes_the_repository() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "graph",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository graph"))
        .stdout(predicate::str::contains("Python files            14"))
        .stdout(predicate::str::contains("Import edges            14"))
        .stdout(predicate::str::contains("Tests                    4"))
        .stdout(predicate::str::contains("Unresolved imports       3"))
        .stdout(predicate::str::contains("Unresolved import details (3)"))
        .stdout(predicate::str::contains(
            "src/payments/stripe.py:1:8  import requests",
        ))
        .stdout(predicate::str::contains(
            "tests/analytics/test_reporting.py:1:8  import analytics.reporting",
        ))
        .stdout(predicate::str::contains(
            "tests/helpers_test.py:1:8  import pytest",
        ));
}

#[test]
fn graph_json_has_a_stable_versioned_schema() {
    let output = json_output(&[
        "--root",
        fixture("src-layout").to_str().expect("UTF-8 fixture"),
        "graph",
        "--json",
    ]);

    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "python_files": 14,
            "modules": 14,
            "import_edges": 14,
            "tests": 4,
            "unresolved_imports": 3,
            "unresolved_import_details": [
                {
                    "importer": "src/payments/stripe.py",
                    "line": 1,
                    "column": 8,
                    "import": {
                        "kind": "import",
                        "module": "requests"
                    }
                },
                {
                    "importer": "tests/analytics/test_reporting.py",
                    "line": 1,
                    "column": 8,
                    "import": {
                        "kind": "import",
                        "module": "analytics.reporting"
                    }
                },
                {
                    "importer": "tests/helpers_test.py",
                    "line": 1,
                    "column": 8,
                    "import": {
                        "kind": "import",
                        "module": "pytest"
                    }
                }
            ]
        })
    );
}

#[test]
fn graph_reports_structured_from_import_diagnostics() {
    let repository = tempdir().expect("temporary repository");
    fs::write(
        repository.path().join("module.py"),
        "\nfrom external.api import Client\n",
    )
    .expect("Python fixture");
    let root = repository.path().to_str().expect("UTF-8 repository");

    urmare()
        .args(["--root", root, "graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "module.py:2:26  from external.api import Client",
        ));

    let output = json_output(&["--root", root, "graph", "--json"]);
    assert_eq!(
        output["unresolved_import_details"],
        json!([{
            "importer": "module.py",
            "line": 2,
            "column": 26,
            "import": {
                "kind": "from",
                "module": "external.api",
                "name": "Client",
                "level": 0
            }
        }])
    );
}

#[test]
fn graph_debug_inspects_module_edges_and_resolution_candidates() {
    let root = fixture("src-layout");
    let root = root.to_str().expect("UTF-8 fixture");

    urmare()
        .args([
            "--root",
            root,
            "graph",
            "--debug",
            "--focus",
            "src/payments/service.py",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Graph inspection"))
        .stdout(predicate::str::contains(
            "src/payments/service.py -> payments.service [source; 2 dependencies; 1 dependents]",
        ))
        .stdout(predicate::str::contains(
            "src/api/checkout.py -> src/payments/service.py",
        ))
        .stdout(predicate::str::contains(
            "via src/api/checkout.py:1:30  from payments.service import create_payment",
        ))
        .stdout(predicate::str::contains(
            "candidates: payments, payments.stripe",
        ))
        .stdout(predicate::str::contains(
            "matched payments.stripe -> src/payments/stripe.py",
        ));

    let output = json_output(&[
        "--root",
        root,
        "graph",
        "--debug",
        "--focus",
        "src/payments/service.py",
        "--json",
    ]);
    assert_eq!(output["inspection"]["focus"], "src/payments/service.py");
    assert_eq!(
        output["inspection"]["modules"][0]["module"],
        "payments.service"
    );
    assert_eq!(
        output["inspection"]["edges"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(
        output["inspection"]["resolution_traces"][0]["candidate_modules"],
        json!(["payments", "payments.stripe"])
    );
    assert_eq!(
        output["inspection"]["resolution_traces"][0]["resolved_modules"],
        json!([
            {"module": "payments", "path": "src/payments/__init__.py"},
            {"module": "payments.stripe", "path": "src/payments/stripe.py"}
        ])
    );
}

#[test]
fn graph_debug_explains_unresolved_and_invalid_relative_imports() {
    let root = fixture("src-layout");
    let output = json_output(&[
        "--root",
        root.to_str().expect("UTF-8 fixture"),
        "graph",
        "--debug",
        "--focus",
        "src/payments/stripe.py",
        "--json",
    ]);
    let trace = &output["inspection"]["resolution_traces"][0];
    assert_eq!(trace["status"], "unresolved");
    assert_eq!(trace["candidate_modules"], json!(["requests"]));
    assert_eq!(trace["resolved_modules"], json!([]));

    let repository = tempdir().expect("temporary repository");
    fs::write(
        repository.path().join("module.py"),
        "from ..outside import value\n",
    )
    .expect("Python fixture");
    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "graph",
            "--debug",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("[invalid relative import]"))
        .stdout(predicate::str::contains(
            "reason: relative import ascends above the importer package",
        ));
}

#[test]
fn graph_focus_requires_debug() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "graph",
            "--focus",
            "src/payments/service.py",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "required arguments were not provided",
        ))
        .stderr(predicate::str::contains("--debug"));
}

#[test]
fn graph_truncates_human_diagnostics_and_json_remains_complete() {
    let repository = tempdir().expect("temporary repository");
    let source = (0..30)
        .map(|index| format!("import external_{index:02}\n"))
        .collect::<String>();
    fs::write(repository.path().join("module.py"), source).expect("Python fixture");
    let root = repository.path().to_str().expect("UTF-8 repository");

    urmare()
        .args(["--root", root, "graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Unresolved imports      30"))
        .stdout(predicate::str::contains("import external_24"))
        .stdout(predicate::str::contains("import external_25").not())
        .stdout(predicate::str::contains(
            "... 5 more (use --all to show everything)",
        ));

    urmare()
        .args(["--root", root, "graph", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("import external_29"))
        .stdout(predicate::str::contains("use --all").not());

    urmare()
        .args(["--root", root, "graph", "--debug"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Import resolution trace (30)"))
        .stdout(predicate::str::contains("import external_24 [unresolved]"))
        .stdout(predicate::str::contains("import external_25 [unresolved]").not());

    urmare()
        .args(["--root", root, "graph", "--debug", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("import external_29 [unresolved]"));

    let output = json_output(&["--root", root, "graph", "--json"]);
    assert_eq!(
        output["unresolved_import_details"]
            .as_array()
            .expect("unresolved import details")
            .len(),
        30
    );

    urmare()
        .args(["--root", root, "graph", "--all", "--json"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn impact_lists_direct_transitive_and_test_results() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "impact",
            "src/payments/stripe.py",
        ])
        .assert()
        .success()
        .stdout(concat!(
            "Impact analysis\n",
            "\n",
            "Changed (1)\n",
            "  src/payments/stripe.py\n",
            "\n",
            "Summary\n",
            "  Directly affected modules      2\n",
            "  Transitively affected modules  1\n",
            "  Affected tests                  2\n",
            "\n",
            "Directly affected modules (2)\n",
            "  src/payments/formatters/card.py\n",
            "  src/payments/service.py\n",
            "\n",
            "Transitively affected modules (1)\n",
            "  src/api/checkout.py\n",
            "\n",
            "Affected tests (2)\n",
            "  tests/api/test_checkout.py\n",
            "  tests/payments/test_stripe.py\n",
        ))
        .stdout(predicate::str::contains("Blast radius").not())
        .stdout(predicate::str::contains("Risk:").not());
}

#[test]
fn explicit_multi_file_impact_unions_results_and_preserves_attribution() {
    let root = fixture("src-layout");
    let root = root.to_str().expect("UTF-8 fixture");

    urmare()
        .args([
            "--root",
            root,
            "impact",
            "src/payments/stripe.py",
            "src/cycles/a.py",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Changed (2)"))
        .stdout(predicate::str::contains("src/cycles/a.py"))
        .stdout(predicate::str::contains("src/payments/stripe.py"))
        .stdout(predicate::str::contains("Directly affected modules (3)"))
        .stdout(predicate::str::contains("src/cycles/b.py"))
        .stdout(predicate::str::contains("caused by src/cycles/a.py"))
        .stdout(predicate::str::contains("caused by src/payments/stripe.py"))
        .stdout(predicate::str::contains("Affected tests (2)"));

    urmare()
        .args([
            "--root",
            root,
            "tests",
            "--affected",
            "src/payments/stripe.py",
            "src/cycles/a.py",
        ])
        .assert()
        .success()
        .stdout("tests/api/test_checkout.py\ntests/payments/test_stripe.py\n");

    let output = json_output(&[
        "--root",
        root,
        "impact",
        "src/payments/stripe.py",
        "src/cycles/a.py",
        "src/payments/stripe.py",
        "--json",
    ]);
    assert_eq!(
        output["changed"],
        json!(["src/cycles/a.py", "src/payments/stripe.py"])
    );
    assert_eq!(
        output["directly_affected"],
        json!([
            "src/cycles/b.py",
            "src/payments/formatters/card.py",
            "src/payments/service.py",
            "tests/payments/test_stripe.py"
        ])
    );
    let cycle_attribution = output["attributions"]
        .as_array()
        .expect("attributions array")
        .iter()
        .find(|attribution| attribution["affected"] == "src/cycles/b.py")
        .expect("cycle attribution");
    assert_eq!(cycle_attribution["caused_by"], json!(["src/cycles/a.py"]));

    let tests = json_output(&[
        "--root",
        root,
        "tests",
        "--affected",
        "src/payments/stripe.py",
        "src/cycles/a.py",
        "--json",
    ]);
    assert_eq!(
        tests["changed"],
        json!(["src/cycles/a.py", "src/payments/stripe.py"])
    );
    assert_eq!(
        tests["affected_tests"],
        json!([
            "tests/api/test_checkout.py",
            "tests/payments/test_stripe.py"
        ])
    );
}

#[test]
fn impact_truncates_large_human_results_and_all_disables_truncation() {
    let repository = tempdir().expect("temporary repository");
    fs::write(repository.path().join("changed.py"), "VALUE = 1\n").expect("changed module");
    for index in 0..30 {
        fs::write(
            repository.path().join(format!("dependent_{index:02}.py")),
            "import changed\n",
        )
        .expect("dependent module");
    }
    let root = repository.path().to_str().expect("UTF-8 repository");

    urmare()
        .args(["--root", root, "impact", "changed.py"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Directly affected modules      30",
        ))
        .stdout(predicate::str::contains("Directly affected modules (30)"))
        .stdout(predicate::str::contains("dependent_24.py"))
        .stdout(predicate::str::contains("dependent_25.py").not())
        .stdout(predicate::str::contains(
            "... 5 more (use --all to show everything)",
        ));

    urmare()
        .args(["--root", root, "impact", "changed.py", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dependent_29.py"))
        .stdout(predicate::str::contains("use --all").not());

    let output = json_output(&["--root", root, "impact", "changed.py", "--json"]);
    assert_eq!(
        output["directly_affected"]
            .as_array()
            .expect("directly affected array")
            .len(),
        30
    );
}

#[test]
fn tests_outputs_only_affected_test_paths() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "tests",
            "--affected",
            "src/payments/stripe.py",
        ])
        .assert()
        .success()
        .stdout("tests/api/test_checkout.py\ntests/payments/test_stripe.py\n")
        .stdout(predicate::str::contains("test_reporting.py").not());
}

#[test]
fn configured_source_roots_work_across_cli_commands() {
    let root = fixture("multiple-roots");
    let root = root.to_str().expect("UTF-8 fixture");
    let changed = "packages/payments/src/payments/pricing.py";

    urmare()
        .args(["--root", root, "graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Python files             7"))
        .stdout(predicate::str::contains("Import edges             6"))
        .stdout(predicate::str::contains("Tests                    3"))
        .stdout(predicate::str::contains("Unresolved imports       0"));

    urmare()
        .args(["--root", root, "impact", changed])
        .assert()
        .success()
        .stdout(predicate::str::contains("Directly affected modules (1)"))
        .stdout(predicate::str::contains("packages/api/src/api/checkout.py"))
        .stdout(predicate::str::contains(
            "Transitively affected modules (0)",
        ))
        .stdout(predicate::str::contains("Affected tests (2)"));

    urmare()
        .args(["--root", root, "tests", "--affected", changed])
        .assert()
        .success()
        .stdout("tests/api/test_checkout.py\ntests/payments/test_pricing.py\n")
        .stdout(predicate::str::contains("test_unrelated.py").not());

    urmare()
        .args(["--root", root, "why", changed, "tests/api/test_checkout.py"])
        .assert()
        .success()
        .stdout(concat!(
            "tests/api/test_checkout.py\n",
            "  -> packages/api/src/api/checkout.py\n",
            "     via tests/api/test_checkout.py:1:17  from api import checkout\n",
            "  -> packages/payments/src/payments/pricing.py\n",
            "     via packages/api/src/api/checkout.py:1:22  from payments import pricing\n",
        ));
}

#[test]
fn configured_test_roots_and_excludes_apply_across_cli_commands() {
    let root = fixture("configured-boundaries");
    let root = root.to_str().expect("UTF-8 fixture");
    let changed = "src/app/core.py";

    urmare()
        .args(["--root", root, "graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Python files             5"))
        .stdout(predicate::str::contains("Import edges             6"))
        .stdout(predicate::str::contains("Tests                    2"))
        .stdout(predicate::str::contains("Unresolved imports       0"));

    let graph = json_output(&["--root", root, "graph", "--json"]);
    assert_eq!(graph["unresolved_imports"], 0);
    assert_eq!(graph["unresolved_import_details"], json!([]));

    urmare()
        .args(["--root", root, "impact", changed])
        .assert()
        .success()
        .stdout(predicate::str::contains("Directly affected modules (1)"))
        .stdout(predicate::str::contains("src/app/service.py"))
        .stdout(predicate::str::contains(
            "Transitively affected modules (0)",
        ))
        .stdout(predicate::str::contains("Affected tests (2)"))
        .stdout(predicate::str::contains("checks/test_conventional.py"))
        .stdout(predicate::str::contains("verification/checkout_spec.py"))
        .stdout(predicate::str::contains("src/generated/client.py").not())
        .stdout(predicate::str::contains("vendor/legacy.py").not());

    urmare()
        .args(["--root", root, "tests", "--affected", changed])
        .assert()
        .success()
        .stdout("checks/test_conventional.py\nverification/checkout_spec.py\n");

    urmare()
        .args([
            "--root",
            root,
            "why",
            changed,
            "verification/checkout_spec.py",
        ])
        .assert()
        .success()
        .stdout(concat!(
            "verification/checkout_spec.py\n",
            "  -> src/app/service.py\n",
            "     via verification/checkout_spec.py:3:17  from app import service\n",
            "  -> src/app/core.py\n",
            "     via src/app/service.py:3:15  from . import core\n",
        ));

    urmare()
        .args(["--root", root, "impact", "src/generated/client.py"])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "Python file `src/generated/client.py` was not indexed",
        ));
}

#[test]
fn configuration_errors_are_actionable() {
    let missing = tempdir().expect("temporary repository");
    fs::write(missing.path().join("module.py"), "VALUE = 1\n").expect("Python fixture");
    fs::write(
        missing.path().join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"missing\"]\n",
    )
    .expect("configuration fixture");
    urmare()
        .args([
            "--root",
            missing.path().to_str().expect("UTF-8 repository"),
            "graph",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "configured `source-roots` path `missing` does not exist",
        ));

    let invalid_boundary = tempdir().expect("temporary repository");
    fs::write(invalid_boundary.path().join("module.py"), "VALUE = 1\n").expect("Python fixture");
    fs::write(
        invalid_boundary.path().join("pyproject.toml"),
        "[tool.urmare]\nexclude = [\"generated/[\"]\n",
    )
    .expect("configuration fixture");
    urmare()
        .args([
            "--root",
            invalid_boundary.path().to_str().expect("UTF-8 repository"),
            "graph",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "invalid exclusion pattern `generated/[`",
        ));

    let missing_test_root = tempdir().expect("temporary repository");
    fs::write(missing_test_root.path().join("module.py"), "VALUE = 1\n").expect("Python fixture");
    fs::write(
        missing_test_root.path().join("pyproject.toml"),
        "[tool.urmare]\ntest-roots = [\"verification\"]\n",
    )
    .expect("configuration fixture");
    urmare()
        .args([
            "--root",
            missing_test_root.path().to_str().expect("UTF-8 repository"),
            "graph",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "configured `test-roots` path `verification` does not exist",
        ));

    let collision = tempdir().expect("temporary repository");
    fs::create_dir_all(collision.path().join("one/pkg")).expect("first source root");
    fs::create_dir_all(collision.path().join("two/pkg")).expect("second source root");
    fs::write(
        collision.path().join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"one\", \"two\"]\n",
    )
    .expect("configuration fixture");
    fs::write(collision.path().join("one/pkg/module.py"), "VALUE = 1\n").expect("first module");
    fs::write(collision.path().join("two/pkg/module.py"), "VALUE = 2\n").expect("second module");
    urmare()
        .args([
            "--root",
            collision.path().to_str().expect("UTF-8 repository"),
            "graph",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("module `pkg.module` maps to both"))
        .stderr(predicate::str::contains("tool.urmare.source-roots"));
}

#[test]
fn impact_json_has_a_stable_versioned_schema() {
    let output = json_output(&[
        "--root",
        fixture("src-layout").to_str().expect("UTF-8 fixture"),
        "impact",
        "src/payments/stripe.py",
        "--json",
    ]);

    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "changed": ["src/payments/stripe.py"],
            "directly_affected": [
                "src/payments/formatters/card.py",
                "src/payments/service.py",
                "tests/payments/test_stripe.py"
            ],
            "transitively_affected": [
                "src/api/checkout.py",
                "tests/api/test_checkout.py"
            ],
            "affected_tests": [
                "tests/api/test_checkout.py",
                "tests/payments/test_stripe.py"
            ],
            "attributions": [
                {
                    "affected": "src/api/checkout.py",
                    "caused_by": ["src/payments/stripe.py"]
                },
                {
                    "affected": "src/payments/formatters/card.py",
                    "caused_by": ["src/payments/stripe.py"]
                },
                {
                    "affected": "src/payments/service.py",
                    "caused_by": ["src/payments/stripe.py"]
                },
                {
                    "affected": "tests/api/test_checkout.py",
                    "caused_by": ["src/payments/stripe.py"]
                },
                {
                    "affected": "tests/payments/test_stripe.py",
                    "caused_by": ["src/payments/stripe.py"]
                }
            ]
        })
    );
}

#[test]
fn affected_tests_json_contains_only_selection_fields_and_attribution() {
    let output = json_output(&[
        "--root",
        fixture("src-layout").to_str().expect("UTF-8 fixture"),
        "tests",
        "--affected",
        "src/payments/stripe.py",
        "--json",
    ]);

    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "changed": ["src/payments/stripe.py"],
            "affected_tests": [
                "tests/api/test_checkout.py",
                "tests/payments/test_stripe.py"
            ],
            "attributions": [
                {
                    "affected": "tests/api/test_checkout.py",
                    "caused_by": ["src/payments/stripe.py"]
                },
                {
                    "affected": "tests/payments/test_stripe.py",
                    "caused_by": ["src/payments/stripe.py"]
                }
            ]
        })
    );
}

#[test]
fn why_prints_the_dependency_path_in_natural_orientation() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "why",
            "src/payments/stripe.py",
            "tests/api/test_checkout.py",
        ])
        .assert()
        .success()
        .stdout(concat!(
            "tests/api/test_checkout.py\n",
            "  -> src/api/checkout.py\n",
            "     via tests/api/test_checkout.py:1:17  from api import checkout\n",
            "  -> src/payments/service.py\n",
            "     via src/api/checkout.py:1:30  from payments.service import create_payment\n",
            "  -> src/payments/stripe.py\n",
            "     via src/payments/service.py:1:15  from . import stripe\n",
        ));
}

#[test]
fn why_json_has_canonical_endpoints_and_the_ordered_path() {
    let root = fixture("src-layout");
    let changed = root.join("src/payments/stripe.py");
    let affected = root.join("tests/api/test_checkout.py");
    let output = json_output(&[
        "--root",
        root.to_str().expect("UTF-8 fixture"),
        "why",
        changed.to_str().expect("UTF-8 changed path"),
        affected.to_str().expect("UTF-8 affected path"),
        "--json",
    ]);

    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "changed": "src/payments/stripe.py",
            "affected": "tests/api/test_checkout.py",
            "path": [
                "tests/api/test_checkout.py",
                "src/api/checkout.py",
                "src/payments/service.py",
                "src/payments/stripe.py"
            ],
            "steps": [
                {
                    "dependent": "tests/api/test_checkout.py",
                    "dependency": "src/api/checkout.py",
                    "imports": [{
                        "line": 1,
                        "column": 17,
                        "import": {
                            "kind": "from",
                            "module": "api",
                            "name": "checkout",
                            "level": 0
                        }
                    }]
                },
                {
                    "dependent": "src/api/checkout.py",
                    "dependency": "src/payments/service.py",
                    "imports": [{
                        "line": 1,
                        "column": 30,
                        "import": {
                            "kind": "from",
                            "module": "payments.service",
                            "name": "create_payment",
                            "level": 0
                        }
                    }]
                },
                {
                    "dependent": "src/payments/service.py",
                    "dependency": "src/payments/stripe.py",
                    "imports": [{
                        "line": 1,
                        "column": 15,
                        "import": {
                            "kind": "from",
                            "module": null,
                            "name": "stripe",
                            "level": 1
                        }
                    }]
                }
            ]
        })
    );
}

#[test]
fn why_returns_nonzero_when_no_dependency_path_exists() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "why",
            "src/payments/stripe.py",
            "tests/analytics/test_reporting.py",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("no dependency path exists"));

    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "why",
            "src/payments/stripe.py",
            "tests/analytics/test_reporting.py",
            "--json",
        ])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::contains("no dependency path exists"));
}

#[test]
fn input_and_repository_errors_are_actionable() {
    urmare()
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "impact",
            "missing.py",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains(
            "input file `missing.py` does not exist",
        ));

    urmare()
        .args([
            "--root",
            fixture("syntax-invalid").to_str().expect("UTF-8 fixture"),
            "graph",
            "--json",
        ])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::contains(
            "unable to parse Python source `broken.py`",
        ));

    urmare()
        .args([
            "--root",
            fixture("no-python").to_str().expect("UTF-8 fixture"),
            "graph",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::contains("no Python files were found"));
}

#[test]
fn git_diff_impact_unions_staged_unstaged_and_untracked_changes_with_attribution() {
    let repository = initialized_git_repository(&[
        ("src/pkg/__init__.py", ""),
        ("src/pkg/a.py", "VALUE = 1\n"),
        ("src/pkg/b.py", "from . import a\n"),
        ("src/pkg/c.py", "VALUE = 1\n"),
        ("tests/test_b.py", "from pkg import b\n"),
        ("tests/test_c.py", "from pkg import c\n"),
    ]);
    fs::write(repository.path().join("src/pkg/a.py"), "VALUE = 2\n").expect("unstaged change");
    fs::write(repository.path().join("src/pkg/c.py"), "VALUE = 2\n").expect("staged change");
    git(repository.path(), &["add", "src/pkg/c.py"]);
    fs::write(repository.path().join("src/pkg/new.py"), "NEW = True\n").expect("untracked change");

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "impact",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Changed (3)"))
        .stdout(predicate::str::contains("src/pkg/a.py"))
        .stdout(predicate::str::contains("src/pkg/c.py"))
        .stdout(predicate::str::contains("src/pkg/new.py"))
        .stdout(predicate::str::contains("Directly affected modules (1)"))
        .stdout(predicate::str::contains(
            "Transitively affected modules (0)",
        ))
        .stdout(predicate::str::contains("Affected tests (2)"))
        .stdout(predicate::str::contains("caused by src/pkg/a.py"))
        .stdout(predicate::str::contains("caused by src/pkg/c.py"));

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "tests",
            "--affected",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .success()
        .stdout("tests/test_b.py\ntests/test_c.py\n");

    let output = json_output(&[
        "--root",
        repository.path().to_str().expect("UTF-8 repository"),
        "impact",
        "--git-diff",
        "HEAD",
        "--json",
    ]);
    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "changed": ["src/pkg/a.py", "src/pkg/c.py", "src/pkg/new.py"],
            "directly_affected": ["src/pkg/b.py", "tests/test_c.py"],
            "transitively_affected": ["tests/test_b.py"],
            "affected_tests": ["tests/test_b.py", "tests/test_c.py"],
            "attributions": [
                {
                    "affected": "src/pkg/b.py",
                    "caused_by": ["src/pkg/a.py"]
                },
                {
                    "affected": "tests/test_b.py",
                    "caused_by": ["src/pkg/a.py"]
                },
                {
                    "affected": "tests/test_c.py",
                    "caused_by": ["src/pkg/c.py"]
                }
            ]
        })
    );
}

#[test]
fn changed_analyzes_the_working_tree_and_discovers_root_from_a_subdirectory() {
    let repository = initialized_git_repository(&[
        ("src/pkg/__init__.py", ""),
        ("src/pkg/core.py", "VALUE = 1\n"),
        ("src/pkg/service.py", "from . import core\n"),
        ("tests/test_service.py", "from pkg import service\n"),
    ]);
    fs::write(repository.path().join("src/pkg/core.py"), "VALUE = 2\n").expect("unstaged change");

    let output = urmare()
        .current_dir(repository.path().join("src/pkg"))
        .args(["impact", "--changed", "--json"])
        .output()
        .expect("Urmare changed output");

    assert!(
        output.status.success(),
        "Urmare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).expect("valid JSON"),
        json!({
            "schema_version": 1,
            "changed": ["src/pkg/core.py"],
            "directly_affected": ["src/pkg/service.py"],
            "transitively_affected": ["tests/test_service.py"],
            "affected_tests": ["tests/test_service.py"],
            "attributions": [
                {
                    "affected": "src/pkg/service.py",
                    "caused_by": ["src/pkg/core.py"]
                },
                {
                    "affected": "tests/test_service.py",
                    "caused_by": ["src/pkg/core.py"]
                }
            ]
        })
    );
}

#[test]
fn changed_reports_when_execution_is_outside_git() {
    let directory = tempdir().expect("temporary directory");
    fs::write(directory.path().join("module.py"), "VALUE = 1\n").expect("Python fixture");

    urmare()
        .current_dir(directory.path())
        .args(["impact", "--changed", "--json"])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::contains("is not a Git repository"));
}

#[test]
fn git_diff_keeps_deleted_and_renamed_modules_explainable() {
    let repository = initialized_git_repository(&[
        ("src/pkg/__init__.py", ""),
        ("src/pkg/deleted.py", "DELETED = True\n"),
        ("src/pkg/deleted_user.py", "from . import deleted\n"),
        ("src/pkg/old.py", "OLD = True\n"),
        ("src/pkg/old_user.py", "from . import old\n"),
        ("tests/test_deleted.py", "from pkg import deleted_user\n"),
        ("tests/test_old.py", "from pkg import old_user\n"),
    ]);
    fs::remove_file(repository.path().join("src/pkg/deleted.py")).expect("deleted module");
    git(
        repository.path(),
        &["mv", "src/pkg/old.py", "src/pkg/renamed.py"],
    );

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "tests",
            "--affected",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .success()
        .stdout("tests/test_deleted.py\ntests/test_old.py\n");
}

#[test]
fn git_diff_keeps_a_deleted_configured_source_root_explainable() {
    let repository = initialized_git_repository(&[
        (
            "pyproject.toml",
            "[tool.urmare]\nsource-roots = [\"packages/payments/src\"]\n",
        ),
        ("packages/payments/src/payments.py", "VALUE = 1\n"),
        ("tests/test_payments.py", "import payments\n"),
    ]);
    fs::remove_dir_all(repository.path().join("packages/payments"))
        .expect("delete configured source root");

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "tests",
            "--affected",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .success()
        .stdout("tests/test_payments.py\n");
}

#[test]
fn git_diff_reports_invalid_bases_and_incomplete_test_selection() {
    let repository = initialized_git_repository(&[("module.py", "VALUE = 1\n")]);

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "impact",
            "--git-diff",
            "missing-reference",
            "--json",
        ])
        .assert()
        .code(3)
        .stdout("")
        .stderr(predicate::str::contains(
            "Git base `missing-reference` does not resolve to a commit",
        ));

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "tests",
            "--affected",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "provide one or more changed files or use `--git-diff <base>`",
        ));

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "impact",
            "module.py",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "impact",
            "module.py",
            "--changed",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));

    urmare()
        .args([
            "--root",
            repository.path().to_str().expect("UTF-8 repository"),
            "tests",
            "--affected",
            "module.py",
            "--git-diff",
            "HEAD",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "provide either changed files or `--git-diff <base>`, not both",
        ));
}

#[test]
fn clean_git_diff_json_keeps_every_array_field_present() {
    let repository = initialized_git_repository(&[("module.py", "VALUE = 1\n")]);

    let impact = json_output(&[
        "--root",
        repository.path().to_str().expect("UTF-8 repository"),
        "impact",
        "--git-diff",
        "HEAD",
        "--json",
    ]);
    assert_eq!(
        impact,
        json!({
            "schema_version": 1,
            "changed": [],
            "directly_affected": [],
            "transitively_affected": [],
            "affected_tests": [],
            "attributions": []
        })
    );

    let changed = json_output(&[
        "--root",
        repository.path().to_str().expect("UTF-8 repository"),
        "impact",
        "--changed",
        "--json",
    ]);
    assert_eq!(changed, impact);

    let tests = json_output(&[
        "--root",
        repository.path().to_str().expect("UTF-8 repository"),
        "tests",
        "--affected",
        "--git-diff",
        "HEAD",
        "--json",
    ]);
    assert_eq!(
        tests,
        json!({
            "schema_version": 1,
            "changed": [],
            "affected_tests": [],
            "attributions": []
        })
    );
}

fn json_output(arguments: &[&str]) -> Value {
    let output = urmare()
        .args(arguments)
        .output()
        .expect("Urmare JSON output");
    assert!(
        output.status.success(),
        "Urmare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "successful JSON wrote to stderr");
    serde_json::from_slice(&output.stdout).expect("valid JSON")
}

fn initialized_git_repository(files: &[(&str, &str)]) -> TempDir {
    let repository = tempdir().expect("temporary Git repository");
    git(repository.path(), &["init", "--quiet"]);
    for (path, contents) in files {
        let path = repository.path().join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, contents).expect("fixture file");
    }
    git(repository.path(), &["add", "."]);
    git(
        repository.path(),
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
    repository
}

fn git(root: &Path, arguments: &[&str]) {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("Git is available for tests");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
}
