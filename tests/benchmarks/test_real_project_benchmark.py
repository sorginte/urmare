from __future__ import annotations

import dataclasses
import importlib.util
import json
import os
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPOSITORY_ROOT / "benchmarks" / "real_projects" / "benchmark.py"
SPEC = importlib.util.spec_from_file_location("real_project_benchmark", MODULE_PATH)
assert SPEC and SPEC.loader
benchmark = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = benchmark
SPEC.loader.exec_module(benchmark)


class ManifestTests(unittest.TestCase):
    def test_reviewed_manifest_parses(self):
        corpus = benchmark.load_corpus()
        self.assertEqual(
            [project.id for project in corpus.projects],
            ["flask", "fastapi", "django", "pandas", "airflow"],
        )
        self.assertTrue(all(len(project.commit) == 40 for project in corpus.projects))
        schema = json.loads(
            (benchmark.SCRIPT_DIRECTORY / "raw-result.schema.json").read_text(encoding="utf-8")
        )
        self.assertEqual(schema["properties"]["schema_version"]["const"], 1)
        for project in corpus.projects:
            if project.configuration_patch is None:
                continue
            configured = tomllib.loads(
                project.configuration_patch.patch_file.read_text(encoding="utf-8")
            )["tool"]["urmare"]
            self.assertEqual(tuple(configured.get("exclude", ())), project.exclusions)
            if "source-roots" in configured:
                self.assertEqual(tuple(configured["source-roots"]), project.source_roots)
            if "test-roots" in configured:
                self.assertEqual(tuple(configured["test-roots"]), project.test_roots)

    def test_rejects_abbreviated_and_invalid_commit_shas(self):
        for invalid in ("22d9247", "g" * 40, "A" * 40):
            with self.subTest(invalid=invalid):
                path = self.modified_manifest("flask", commit=invalid)
                with self.assertRaisesRegex(benchmark.BenchmarkError, "full lowercase"):
                    benchmark.load_corpus(path)

    def test_rejects_invalid_changed_file_and_command(self):
        path = self.modified_manifest("flask", changed_file="../app.py")
        with self.assertRaisesRegex(benchmark.BenchmarkError, "repository-relative"):
            benchmark.load_corpus(path)
        path = self.modified_manifest("flask", benchmark_command="urmare impact anything.py")
        with self.assertRaisesRegex(benchmark.BenchmarkError, "must be exactly"):
            benchmark.load_corpus(path)

    def modified_manifest(self, project_id: str, **changes):
        document = json.loads(benchmark.DEFAULT_MANIFEST.read_text(encoding="utf-8"))
        for project in document["projects"]:
            if project["id"] == project_id:
                project.update(changes)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)
        path = directory / "corpus.json"
        path.write_text(json.dumps(document), encoding="utf-8")
        return path


class SafetyAndSerializationTests(unittest.TestCase):
    def test_exact_command_construction(self):
        project = benchmark.load_corpus().projects[0]
        provenance = benchmark.Provenance(
            Path("/opt/urmare"), "0" * 64, "0.1.1", "1" * 40, Path("/opt/helper")
        )
        self.assertEqual(
            benchmark.exact_cli_command(provenance, project),
            [
                "/opt/urmare",
                "--root",
                "repository",
                "impact",
                "src/flask/app.py",
                "--json",
            ],
        )

    def test_cache_roots_are_empty_isolated_and_contained(self):
        with tempfile.TemporaryDirectory() as temporary:
            sample = Path(temporary)
            environment, root = benchmark.cli_cache_environment(sample)
            self.assertTrue(Path(environment["XDG_CACHE_HOME"]).is_relative_to(sample))
            self.assertEqual(list(root.rglob("*.*")), [])
            cache_file = Path(environment["XDG_CACHE_HOME"]) / "org.Sorginte.Urmare" / "index"
            cache_file.parent.mkdir(parents=True)
            cache_file.write_text("index", encoding="utf-8")
            benchmark.assert_cache_contained(sample, root)

    def test_refuses_to_delete_outside_owned_root(self):
        with tempfile.TemporaryDirectory() as owner, tempfile.TemporaryDirectory() as outside:
            with self.assertRaisesRegex(benchmark.BenchmarkError, "outside"):
                benchmark.safe_rmtree(Path(owner), Path(outside))
            with self.assertRaisesRegex(benchmark.BenchmarkError, "broad"):
                benchmark.safe_rmtree(Path(owner), Path(owner))

    def test_binary_sha256(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "binary"
            path.write_bytes(b"abc")
            self.assertEqual(
                benchmark.sha256_file(path),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            )

    def test_raw_serialization_and_interrupted_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "raw.jsonl"
            path.touch()
            benchmark.append_json_line(path, {"schema_version": 1, "value": "ok"})
            self.assertEqual(benchmark.read_json_lines(path)[0]["value"], "ok")
            with path.open("ab") as stream:
                stream.write(b'{"partial":')
            with self.assertRaisesRegex(benchmark.BenchmarkError, "partial JSON line"):
                benchmark.read_json_lines(path)

    def test_partial_sample_is_refused_on_resume(self):
        records = [
            {
                "schema_version": 1,
                "record_type": "sample",
                "run_id": "run",
                "project_id": "flask",
                "sample_number": 1,
                "scenario": "cold",
                "sample_role": "measurement",
                "measured": True,
            }
        ]
        with self.assertRaisesRegex(benchmark.BenchmarkError, "ambiguous partial"):
            benchmark.completed_samples(records, "run")

    def test_result_hash_is_stable_across_key_order_and_omitted_null(self):
        first = {"schema_version": 1, "changed": ["a.py"], "full_validation": None}
        second = {"changed": ["a.py"], "schema_version": 1}
        self.assertEqual(benchmark.result_hash(first), benchmark.result_hash(second))


class SummaryTests(unittest.TestCase):
    def test_statistics_use_deterministic_interpolation(self):
        summary = benchmark.statistics([1, 2, 3, 4])
        self.assertEqual(summary["sample_count"], 4)
        self.assertEqual(summary["median"], 2.5)
        self.assertEqual(summary["p25"], 1.75)
        self.assertEqual(summary["p75"], 3.25)
        self.assertEqual(summary["minimum"], 1)
        self.assertEqual(summary["maximum"], 4)

    def test_summary_separates_warmup_end_to_end_internal_and_query(self):
        records = [self.record(False, 999), self.record(True, 100)]
        summary = benchmark.summarize_records(records)
        self.assertEqual(summary["discarded_warm_up_records"], 1)
        group = summary["groups"][0]
        self.assertEqual(group["latency_ns"]["end_to_end_cli"]["median"], 100.0)
        self.assertEqual(group["latency_ns"]["internal_index_update"]["median"], 20.0)
        self.assertEqual(group["latency_ns"]["impact_query"]["median"], 5.0)
        self.assertIn("files_parsed", group["work_counters"])

    @staticmethod
    def record(measured: bool, elapsed: int):
        return {
            "schema_version": 1,
            "record_type": "sample",
            "run_id": "run",
            "project_id": "flask",
            "sample_number": 1,
            "scenario": "warm",
            "sample_role": "measurement" if measured else "warm_up",
            "measured": measured,
            "valid": True,
            "end_to_end": {"elapsed_ns": elapsed},
            "internal_timings_ns": {"index_total": 20},
            "query_profile": {"impact_query": 5, "query_total": 7},
            "internal_work": {"files_parsed": 0, "build_kind": "reused"},
            "normalized_result_sha256": "a" * 64,
        }


class RepositoryValidationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.work_root = benchmark.ensure_owned_root(Path(self.temporary.name) / "work", create=True)
        self.project = benchmark.create_smoke_project(self.work_root)

    def tearDown(self):
        self.temporary.cleanup()

    def test_deterministic_edit_preserves_imports(self):
        sample = self.work_root / "samples" / "edit"
        repository = benchmark.create_checkout(self.work_root, sample, self.project)
        self.assertEqual(benchmark.git_changes(repository), set())
        self.assertEqual(
            benchmark.run_command(["git", "rev-parse", "HEAD^"], cwd=repository).stdout.strip(),
            self.project.commit,
        )
        path = repository / self.project.changed_file
        imports_before = [line for line in path.read_text().splitlines() if line.startswith(("import ", "from "))]
        benchmark.apply_content_only_edit(repository, self.project)
        imports_after = [line for line in path.read_text().splitlines() if line.startswith(("import ", "from "))]
        self.assertEqual(imports_before, imports_after)
        self.assertTrue(path.read_text().endswith(benchmark.BENCHMARK_EDIT))

    def test_configuration_commit_is_deterministic(self):
        first = benchmark.create_checkout(
            self.work_root, self.work_root / "samples" / "first", self.project
        )
        second = benchmark.create_checkout(
            self.work_root, self.work_root / "samples" / "second", self.project
        )
        self.assertEqual(benchmark.git_head(first), benchmark.git_head(second))

    def test_dirty_and_wrong_revision_are_rejected(self):
        sample = self.work_root / "samples" / "dirty"
        repository = benchmark.create_checkout(self.work_root, sample, self.project)
        (repository / "untracked.py").write_text("VALUE = 1\n", encoding="utf-8")
        with self.assertRaisesRegex(benchmark.BenchmarkError, "unexpected checkout change"):
            benchmark.verify_checkout(
                repository, self.project, expected_changes=set(), configured=True
            )
        wrong = dataclasses.replace(self.project, commit="0" * 40)
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark.verify_mirror(self.work_root, wrong)
        missing = dataclasses.replace(self.project, changed_file="src/demo/missing.py")
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark.verify_mirror(self.work_root, missing)

    def test_wrong_configuration_patch_is_rejected(self):
        assert self.project.configuration_patch
        self.project.configuration_patch.patch_file.write_text(
            "[tool.urmare]\nsource-roots = [\"wrong\"]\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(benchmark.BenchmarkError, "result hash"):
            benchmark.verify_mirror(self.work_root, self.project)

    def test_malformed_non_bare_mirror_is_rejected(self):
        malformed = dataclasses.replace(self.project, id="malformed")
        path = benchmark.mirror_path(self.work_root, malformed)
        path.mkdir(parents=True)
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark.verify_mirror(self.work_root, malformed)


@unittest.skipIf(os.name == "nt", "executable test helpers use POSIX shebangs")
class OfflineSmokeLifecycleTests(unittest.TestCase):
    def test_complete_smoke_lifecycle_and_raw_schema(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary, helper = self.fake_tools(root)
            work = root / "work"
            output = root / "raw.jsonl"
            status = benchmark.main(
                [
                    "smoke",
                    "--work-dir",
                    os.fspath(work),
                    "--binary",
                    os.fspath(binary),
                    "--profile-helper",
                    os.fspath(helper),
                    "--output",
                    os.fspath(output),
                ]
            )
            self.assertEqual(status, 0)
            records = benchmark.read_json_lines(output)
            schema = json.loads(
                (benchmark.SCRIPT_DIRECTORY / "raw-result.schema.json").read_text(encoding="utf-8")
            )
            self.assertEqual(len(records), 4)
            self.assertEqual(sum(record["measured"] for record in records), 3)
            self.assertEqual(
                {(record["scenario"], record["sample_role"]) for record in records},
                benchmark.expected_record_keys(),
            )
            for record in records:
                self.assertEqual(set(record), set(schema["required"]))
                self.assertIn("elapsed_ns", record["end_to_end"])
                self.assertIn("index_total", record["internal_timings_ns"])
                self.assertIn("impact_query", record["query_profile"])
                self.assertEqual(record["end_to_end"]["exit_status"], 0)
                self.assertRegex(record["normalized_result_sha256"], r"^[0-9a-f]{64}$")

    @staticmethod
    def fake_tools(root: Path):
        commit = benchmark.git_head(REPOSITORY_ROOT)
        binary = root / "urmare"
        binary.write_text(
            """#!/usr/bin/env python3
import json, os, pathlib, sys
if sys.argv[1:] == ["--version"]:
    print("urmare 0.1.1")
    raise SystemExit(0)
cache = pathlib.Path(os.environ["XDG_CACHE_HOME"]) / "fake" / "index"
cache.parent.mkdir(parents=True, exist_ok=True)
cache.write_text("present")
changed = sys.argv[-2]
print(json.dumps({
    "schema_version": 1,
    "changed": [changed],
    "directly_affected": ["src/demo/service.py"],
    "transitively_affected": ["tests/test_service.py"],
    "affected_tests": ["tests/test_service.py"],
    "attributions": [
        {"affected": "src/demo/service.py", "caused_by": [changed]},
        {"affected": "tests/test_service.py", "caused_by": [changed]},
    ],
}, sort_keys=True))
""",
            encoding="utf-8",
        )
        helper = root / "profile_repository"
        helper.write_text(
            f"""#!/usr/bin/env python3
import json, pathlib, sys
args = sys.argv[1:]
root = pathlib.Path(args[args.index("--root") + 1])
changed = args[args.index("--changed") + 1]
uncached = "--uncached" in args
edited = "urmare-benchmark" in (root / changed).read_text()
if uncached:
    kind, fallback, read, parsed = "full", "cache_disabled", 4, 4
else:
    cache = pathlib.Path(args[args.index("--cache") + 1])
    state = cache / "state"
    if not state.exists():
        kind, fallback, read, parsed = "full", "missing_index", 4, 4
    elif edited and state.read_text() == "original":
        kind, fallback, read, parsed = "incremental", None, 1, 1
    else:
        kind, fallback, read, parsed = "reused", None, 0, 0
    cache.mkdir(parents=True, exist_ok=True)
    state.write_text("edited" if edited else "original")
result = {{
    "schema_version": 1,
    "changed": [changed],
    "directly_affected": ["src/demo/service.py"],
    "transitively_affected": ["tests/test_service.py"],
    "affected_tests": ["tests/test_service.py"],
    "attributions": [
        {{"affected": "src/demo/service.py", "caused_by": [changed]}},
        {{"affected": "tests/test_service.py", "caused_by": [changed]}},
    ],
}}
work = {{
    "build_kind": kind, "fallback_reason": fallback,
    "directories_inspected": 4 if kind == "full" else 0,
    "inventory_entries_inspected": 4,
    "files_statted": read, "files_read": read, "files_hashed": read, "files_parsed": parsed,
    "importers_reresolved": 0, "index_records_written": 1 if kind == "incremental" else (4 if kind == "full" else 0),
    "index_records_read": 1 if kind == "incremental" else 0,
    "bytes_written": 1 if kind in ("full", "incremental") else 0,
    "modules_added": 0, "modules_removed": 0, "modules_remapped": 0,
    "modules_reused": 1 if kind == "incremental" else 0,
    "records_added": 0, "records_removed": 0,
    "forward_edges_added": 0, "forward_edges_removed": 0,
    "reverse_edges_added": 0, "reverse_edges_removed": 0,
}}
print(json.dumps({{
    "schema_version": 1,
    "urmare_version": "0.1.1",
    "build": {{"git_commit": "{commit}", "rustc_version": "rustc 1.95.0", "profile": "release"}},
    "repository": {{"python_files": 4, "modules": 4, "import_edges": 2, "tests": 1, "unresolved_imports": 0}},
    "internal_timings_ns": {{"index_load": 1, "git_delta_detection": 2, "update": 3, "persistence": 4, "index_total": 10}},
    "internal_work": work,
    "query_profile": {{"index_open": 1, "impact_query": 2, "recovery_fallback": 0, "query_total": 3, "records_read": 2}},
    "result": result,
}}, sort_keys=True))
""",
            encoding="utf-8",
        )
        binary.chmod(binary.stat().st_mode | 0o111)
        helper.chmod(helper.stat().st_mode | 0o111)
        return binary, helper


if __name__ == "__main__":
    unittest.main()
