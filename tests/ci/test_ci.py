"""Offline Action, release-installer, consumer, and candidate-binary coverage."""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts" / "ci"))
sys.path.insert(0, str(REPO / "examples" / "ci"))
import install
import plan
import run_pytest as consumer


def make_plan(mode="selective", targets=None, reason="configuration_changed"):
    targets = ["tests/test_service.py"] if targets is None else targets
    result = dict(schema_version=1, changed=["src/service.py"], directly_affected=[],
                  transitively_affected=[], affected_tests=targets,
                  attributions=[], validation=dict(mode=mode, steps=[]))
    if mode != "none":
        result["validation"]["steps"] = [dict(kind="pytest", program="pytest",
                                             args=targets if mode == "selective" else [])]
    if mode == "full":
        result["full_validation"] = dict(required=True, reason=reason,
                                         configuration_paths=["pyproject.toml"])
    return result


class Sandbox(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="urmare ci ;$ ")
        self.addCleanup(self.temporary.cleanup)
        self.temp = Path(self.temporary.name).resolve()
        self.root = self.temp / "repository with spaces;$"
        shutil.copytree(REPO / "fixtures/python-projects/ci-project", self.root)
        self.output = self.temp / "plan ;$`.json"
        self.env = {**os.environ, "GITHUB_WORKSPACE": str(self.root),
                    "RUNNER_TEMP": str(self.temp), "URMARE_VERSION": "0.4.0",
                    "URMARE_BASE": "HEAD", "URMARE_ROOT": "",
                    "URMARE_PLAN_PATH": str(self.output), "URMARE_BINARY": "",
                    "GITHUB_OUTPUT": str(self.temp / "outputs"),
                    "GITHUB_STEP_SUMMARY": str(self.temp / "summary")}

    def write_plan(self, value):
        self.output.write_text(json.dumps(value), encoding="utf-8")
        return self.output

    def git(self, *args, root=None):
        result = subprocess.run(["git", "-C", str(root or self.root), *args],
                                capture_output=True, text=True, check=True)
        return result.stdout.strip()

    def init(self):
        self.git("init", "--quiet")
        self.git("config", "user.name", "CI Test")
        self.git("config", "user.email", "ci@example.invalid")
        self.commit()
        return self.git("rev-parse", "HEAD")

    def commit(self):
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "-m", "fixture")


class InputTests(Sandbox):
    def test_defaults_and_explicit_root(self):
        self.env["URMARE_PLAN_PATH"] = ""
        workspace, root, base, version, temp, output, override = plan.inputs(self.env)
        self.assertEqual((workspace, root, base, version, temp, override),
                         (self.root, self.root, "HEAD", "0.4.0", self.temp, ""))
        self.assertEqual(output.parent, self.temp)
        self.assertTrue(output.name.startswith("urmare-plan-"))
        self.env["URMARE_ROOT"] = "src"
        self.assertEqual(plan.inputs(self.env)[1], self.root / "src")
        self.env["URMARE_ROOT"] = str(self.root)
        self.assertEqual(plan.inputs(self.env)[1], self.root)
        for name in ("URMARE_VERSION", "URMARE_BASE"):
            env = dict(self.env)
            del env[name]
            with self.assertRaises(ValueError):
                plan.inputs(env)

    def test_bad_inputs(self):
        for name, values in {
            "URMARE_BASE": ["", " ", "--help", "HEAD\nmode=none", "HEAD\0"],
            "URMARE_ROOT": ["missing", "bad\nroot"],
            "URMARE_VERSION": ["", "latest", "v0.5.0", "01.2.3", "0.5.0-rc.1", "1.2.3/../../x"],
            "URMARE_PLAN_PATH": ["relative.json", str(self.root / "plan.json"),
                                  str(self.temp / "missing" / "plan.json"), "a\nb"],
        }.items():
            for value in values:
                with self.subTest(name=name, value=value), self.assertRaises((ValueError, OSError)):
                    plan.inputs({**self.env, name: value})

    def test_never_overwrite_or_follow_output_symlink(self):
        self.output.write_text("unrelated")
        with self.assertRaises(ValueError):
            plan.inputs(self.env)
        self.assertEqual(self.output.read_text(), "unrelated")
        link = self.temp / "link.json"
        try:
            link.symlink_to(self.output)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaises(ValueError):
            plan.inputs({**self.env, "URMARE_PLAN_PATH": str(link)})

    def test_output_parent_symlink_cannot_enter_root(self):
        link = self.temp / "repository-link"
        try:
            link.symlink_to(self.root, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaises(ValueError):
            plan.inputs({**self.env, "URMARE_PLAN_PATH": str(link / "new.json")})
        with self.assertRaises(ValueError):
            plan.inputs({**self.env, "RUNNER_TEMP": str(self.root)})

    def test_exact_argument_array(self):
        self.assertEqual(plan.cli_command("/tmp/a binary", self.root, "branch;$`"),
                         ["/tmp/a binary", "--root", str(self.root), "plan",
                          "--git-diff", "branch;$`", "--json"])

    def test_outputs_and_markdown_cannot_inject(self):
        plan.outputs(self.env["GITHUB_OUTPUT"], {"plan-path": "x%0A;`<x>"})
        contents = Path(self.env["GITHUB_OUTPUT"]).read_text()
        lines = contents.splitlines()
        self.assertEqual(lines[0].split("<<")[1], lines[2])
        self.assertEqual(lines[1], "x%0A;`<x>")
        with self.assertRaises(ValueError):
            plan.outputs(self.env["GITHUB_OUTPUT"], {"plan-path": "a\nmode=none"})
        self.assertEqual(plan.code('<img src=x>\n[link](x)|`&'),
                         "<code>&#60;img src&#61;x&#62;&#10;&#91;link&#93;&#40;x&#41;&#124;&#96;&#38;</code>")


class InstallerTests(Sandbox):
    def test_platform_matrix(self):
        for system, machine, target in [
            ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
            ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
            ("Darwin", "arm64", "aarch64-apple-darwin"),
            ("Darwin", "x86_64", "x86_64-apple-darwin"),
            ("Windows", "AMD64", "x86_64-pc-windows-msvc"),
        ]:
            self.assertEqual(install.release_target(system, machine, "glibc"), target)
        for system, machine, libc in [("FreeBSD", "x86_64", ""),
                                     ("Windows", "ARM64", ""),
                                     ("Linux", "riscv64", "glibc"),
                                     ("Linux", "x86_64", "musl")]:
            with self.assertRaises(ValueError):
                install.release_target(system, machine, libc)

    def test_version_and_override_bypass_downloads(self):
        for version in ("0.4.0", "0.5.0", "1.23.456"):
            self.assertEqual(install.version_string(version), version)
        executable = self.root / "candidate"
        executable.write_bytes(b"candidate")
        result = subprocess.CompletedProcess([], 0, b"urmare 0.4.0\n", b"")
        with patch.object(install, "download") as download, patch.object(
                install.subprocess, "run", return_value=result) as run:
            self.assertEqual(install.install("0.4.0", "candidate", self.temp, self.root), executable)
            download.assert_not_called()
            self.assertEqual(run.call_args.args[0], [str(executable), "--version"])
            result.stdout = b"urmare 0.5.0\n"
            with self.assertRaises(ValueError):
                install.install("0.4.0", str(executable), self.temp, self.root)
        with self.assertRaises(OSError):
            install.install("0.4.0", "missing", self.temp, self.root)
        with patch.object(install, "download") as download:
            with self.assertRaises(ValueError):
                install.install("latest", "candidate", self.temp, self.root)
            download.assert_not_called()
        with patch.object(install.subprocess, "run", side_effect=subprocess.CalledProcessError(3, [])):
            with self.assertRaises(subprocess.CalledProcessError):
                install.verify_binary(executable, "0.4.0")

    def archive(self, suffix=".tar.gz", entries=None):
        package = "urmare-v0.4.0-x86_64-unknown-linux-gnu"
        entries = [(package + "/urmare", b"binary", "file")] if entries is None else entries
        path = self.temp / (package + suffix)
        if suffix == ".zip":
            with zipfile.ZipFile(path, "w") as archive:
                for name, data, kind in entries:
                    member = zipfile.ZipInfo(name)
                    member.external_attr = ((stat.S_IFLNK if kind == "link" else stat.S_IFREG) | 0o755) << 16
                    archive.writestr(member, data)
        else:
            with tarfile.open(path, "w:gz") as archive:
                for name, data, kind in entries:
                    member = tarfile.TarInfo(name)
                    member.size = len(data)
                    if kind == "link":
                        member.type = tarfile.SYMTYPE
                        member.linkname = "/etc/passwd"
                    elif kind == "hardlink":
                        member.type = tarfile.LNKTYPE
                    elif kind == "device":
                        member.type = tarfile.CHRTYPE
                    archive.addfile(member, io.BytesIO(data))
        return path, package

    def test_checksum_and_safe_extract_both_formats(self):
        for suffix in (".tar.gz", ".zip"):
            archive, package = self.archive(suffix)
            sums = self.temp / "SHA256SUMS"
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            sums.write_text(f"{digest}  {archive.name}\n")
            install.verify_checksum(archive, sums)
            dest = self.temp / suffix[1:]
            dest.mkdir()
            self.assertEqual(install.extract_binary(archive, dest, package, "urmare").read_bytes(), b"binary")
            for contents in (f"{'0'*64}  {archive.name}\n", "malformed", "",
                             f"{digest}  {archive.name}\n" * 2):
                sums.write_text(contents)
                with self.assertRaises(ValueError):
                    install.verify_checksum(archive, sums)

    def test_unsafe_members_rejected_before_any_write(self):
        package = "urmare-v0.4.0-x86_64-unknown-linux-gnu"
        for suffix in (".tar.gz", ".zip"):
            for name, kind in [("../escape", "file"), ("/absolute", "file"),
                               (package + "/../../escape", "file"),
                               (package + "/./urmare", "file"),
                               (package + "//urmare", "file"),
                               (package + "/bad\\path", "file"), ("C:/escape", "file"),
                               (package + "/urmare", "link")]:
                with self.subTest(suffix=suffix, name=name, kind=kind):
                    archive, package = self.archive(suffix, [(name, b"", kind)])
                    with self.assertRaises(ValueError):
                        install.extract_binary(archive, self.temp, package, "urmare")
                    self.assertFalse((self.temp / "urmare").exists())
        for kind in ("hardlink", "device"):
            archive, package = self.archive(entries=[(package + "/urmare", b"", kind)])
            with self.assertRaises(ValueError):
                install.extract_binary(archive, self.temp, package, "urmare")
        for suffix in (".tar.gz", ".zip"):
            for entries in ([(package + "/LICENSE", b"license", "file")],
                            [(package + "/urmare", b"one", "file"),
                             (package + "/urmare", b"two", "file")]):
                archive, package = self.archive(suffix, entries)
                with self.assertRaises(ValueError):
                    install.extract_binary(archive, self.temp, package, "urmare")

    def test_mocked_release_download_provenance_and_order(self):
        archive, package = self.archive()
        data = archive.read_bytes()
        archive.unlink()
        urls = []
        def download(url, dest):
            urls.append(url)
            dest.write_bytes((hashlib.sha256(data).hexdigest() + "  " + package + ".tar.gz\n").encode()
                             if url.endswith("SHA256SUMS") else data)
        with patch.object(install, "release_target", return_value="x86_64-unknown-linux-gnu"), \
                patch.object(install, "download", side_effect=download), \
                patch.object(install, "verify_binary", side_effect=lambda path, version: path):
            self.assertEqual(install.install("0.4.0", "", self.temp, self.root).read_bytes(), b"binary")
        self.assertEqual(urls, ["https://github.com/sorginte/urmare/releases/download/v0.4.0/" + name
                               for name in ("SHA256SUMS", package + ".tar.gz")])

    def test_bad_download_never_executes_binary(self):
        package = "urmare-v0.4.0-x86_64-unknown-linux-gnu.tar.gz"
        def download(url, dest):
            dest.write_bytes(("0" * 64 + "  " + package + "\n").encode()
                             if url.endswith("SHA256SUMS") else b"bad archive")
        with patch.object(install, "release_target", return_value="x86_64-unknown-linux-gnu"), \
                patch.object(install, "download", side_effect=download), \
                patch.object(install, "verify_binary") as verify:
            with self.assertRaisesRegex(ValueError, "SHA256SUMS"):
                install.install("0.4.0", "", self.temp, self.root)
            verify.assert_not_called()


class ConsumerTests(Sandbox):
    def test_script_exit_status_fails_the_workflow(self):
        harness = (
            "import runpy, subprocess, sys\n"
            "from unittest.mock import patch\n"
            "sys.argv = sys.argv[1:]\n"
            "with patch('subprocess.run', return_value=subprocess.CompletedProcess([], 6)):\n"
            "    runpy.run_path(sys.argv[0], run_name='__main__')\n"
        )
        for value, expected in ((make_plan(), 6), (make_plan("none", []), 0), ({}, 1)):
            result = subprocess.run([sys.executable, "-c", harness,
                                     str(REPO / "examples/ci/run_pytest.py"),
                                     str(self.write_plan(value)), str(self.root)],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, expected, result.stderr)

    def test_all_modes_arguments_and_failure_propagation(self):
        unusual = "-test file ;$`&.py"
        (self.root / unusual).write_text("")
        value = make_plan(targets=["tests/test_service.py", unusual])
        value["validation"]["steps"] = [dict(kind="pytest", program="pytest", args=[target])
                                         for target in value["affected_tests"]]
        with patch.object(consumer.subprocess, "run", side_effect=[
                subprocess.CompletedProcess([], 5), subprocess.CompletedProcess([], 0)]) as run:
            self.assertEqual(consumer.run_plan(self.write_plan(value), self.root), 5)
            self.assertEqual([call.args[0] for call in run.call_args_list], [
                ["uv", "run", "pytest", "--", "./tests/test_service.py"],
                ["uv", "run", "pytest", "--", "./" + unusual]])
            self.assertTrue(all(call.kwargs["cwd"] == self.root for call in run.call_args_list))
        for targets in ([], ["tests/test_service.py"]):
            with patch.object(consumer.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run:
                self.assertEqual(consumer.run_plan(self.write_plan(make_plan("full", targets)), self.root), 0)
                self.assertEqual(run.call_args.args[0], ["uv", "run", "pytest"])
        with patch.object(consumer.subprocess, "run") as run:
            self.assertEqual(consumer.run_plan(self.write_plan(make_plan("none", [])), self.root), 0)
            run.assert_not_called()

    def test_malformed_and_inconsistent_plans_fail_before_execution(self):
        mutations = [
            lambda p: p.update(schema_version=True), lambda p: p.update(schema_version=2),
            lambda p: p.update(affected_tests="test.py"), lambda p: p.update(attributions={}),
            lambda p: p["validation"].update(mode="unknown"),
            lambda p: p["validation"].update(mode="none"),
            lambda p: p["validation"].update(mode="full"),
            lambda p: p["validation"].update(steps=[]),
            lambda p: p["validation"].update(steps="pytest"),
            lambda p: p["validation"]["steps"][0].update(program="sh"),
            lambda p: p["validation"]["steps"][0].update(kind="shell"),
            lambda p: p["validation"]["steps"][0].update(args=[42]),
            lambda p: p["validation"]["steps"][0].update(args=[]),
            lambda p: p["validation"]["steps"].append(dict(kind="pytest", program="sh", args=[])),
            lambda p: p.update(full_validation=None), lambda p: p.update(extra="unexpected"),
            lambda p: p.pop("changed"),
        ]
        for mutate in mutations:
            value = make_plan()
            mutate(value)
            with self.subTest(value=value), patch.object(consumer.subprocess, "run") as run:
                with self.assertRaises((consumer.PlanError, OSError)):
                    consumer.run_plan(self.write_plan(value), self.root)
                run.assert_not_called()
        for content in ("", "{", "[]", '{"schema_version":1,"schema_version":1}'):
            self.output.write_text(content)
            with self.assertRaises((ValueError, OSError)):
                consumer.load_plan(self.output, self.root)

    def test_full_reasons_and_inconsistency(self):
        for reason in ("configuration_changed", "unresolved_dynamic_import",
                       "unsupported_pytest_relationship", "unsupported_exports"):
            self.assertEqual(consumer.load_plan(self.write_plan(make_plan("full", reason=reason)),
                                                self.root)["validation"]["mode"], "full")
        for mutate in [lambda p: p["full_validation"].update(reason="unknown"),
                       lambda p: p["full_validation"].update(required=False),
                       lambda p: p["full_validation"].update(configuration_paths=[]),
                       lambda p: p["validation"]["steps"][0].update(args=["tests/test_service.py"]),
                       lambda p: p["validation"].update(steps=[])]:
            value = make_plan("full")
            mutate(value)
            with self.assertRaises(consumer.PlanError):
                consumer.load_plan(self.write_plan(value), self.root)

    def test_path_escapes_options_nodeids_and_symlinks(self):
        for target in ("../outside.py", "/tmp/test.py", "C:/test.py", "tests\\test.py",
                       "tests/../test.py", "tests//test.py", "./test.py", "test.py::case",
                       "test.py\nmode=none", "test.py\0", "missing.py", "tests"):
            with self.subTest(target=target), self.assertRaises((consumer.PlanError, OSError)):
                consumer.load_plan(self.write_plan(make_plan(targets=[target])), self.root)
        outside = self.temp / "outside.py"
        outside.write_text("")
        try:
            (self.root / "test_link.py").symlink_to(outside)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaises(consumer.PlanError):
            consumer.load_plan(self.write_plan(make_plan(targets=["test_link.py"])), self.root)


class PlannerTests(Sandbox):
    def setUp(self):
        super().setUp()
        self.init()

    def invoke_fake(self, stdout, status=0, interrupt=False):
        def run(args, **kwargs):
            if args[0] == "git":
                return self.real_run(args, **kwargs)
            self.assertEqual(args, ["candidate", "--root", str(self.root), "plan",
                                    "--git-diff", "HEAD", "--json"])
            self.assertFalse(self.output.exists())
            kwargs["stdout"].write(stdout)
            if interrupt:
                raise KeyboardInterrupt()
            return subprocess.CompletedProcess(args, status, stderr=b"::warning::untrusted\n")
        self.real_run = subprocess.run
        with patch.object(plan, "install", return_value="candidate"), \
                patch.object(plan.subprocess, "run", side_effect=run):
            return plan.main(self.env)

    def test_atomic_exact_plan_and_summary(self):
        raw = (json.dumps(make_plan(), indent=3) + "\n").encode()
        before = {p.relative_to(self.root): p.read_bytes() for p in self.root.rglob("*") if p.is_file()}
        self.assertEqual(self.invoke_fake(raw), 0)
        self.assertEqual(self.output.read_bytes(), raw)
        self.assertEqual(before, {p.relative_to(self.root): p.read_bytes()
                                  for p in self.root.rglob("*") if p.is_file()})
        summary = Path(self.env["GITHUB_STEP_SUMMARY"]).read_text()
        self.assertIn(self.git("rev-parse", "HEAD"), summary)
        self.assertIn("Changed Python files: 1; affected test files: 1", summary)
        self.assertIn("mode<<", Path(self.env["GITHUB_OUTPUT"]).read_text())

    def test_nonzero_malformed_and_interrupted_leave_no_plan_or_success_outputs(self):
        for raw, status, interrupt in [(b"{}", 0, False), (b"{", 0, False),
                                       (json.dumps(make_plan()).encode(), 7, False),
                                       (b'{"schema_version":1', 0, True)]:
            with self.subTest(status=status, interrupt=interrupt):
                if interrupt:
                    with self.assertRaises(KeyboardInterrupt):
                        self.invoke_fake(raw, status, interrupt)
                elif status:
                    self.assertEqual(self.invoke_fake(raw, status), 7)
                else:
                    with self.assertRaises(ValueError):
                        self.invoke_fake(raw, status)
                self.assertFalse(self.output.exists())
                self.assertFalse(Path(self.env["GITHUB_OUTPUT"]).exists())
                self.assertFalse(list(self.temp.glob(".urmare-plan-*")))

    def test_summary_failure_and_output_race_never_overwrite(self):
        self.env["GITHUB_STEP_SUMMARY"] = str(self.temp / "missing" / "summary")
        with self.assertRaises(OSError):
            self.invoke_fake(json.dumps(make_plan()).encode())
        self.assertFalse(self.output.exists())
        self.env["GITHUB_STEP_SUMMARY"] = str(self.temp / "summary")
        original = os.link
        def racing_link(source, destination):
            destination.write_text("unrelated race winner")
            return original(source, destination)
        with patch.object(plan.os, "link", side_effect=racing_link), self.assertRaises(FileExistsError):
            self.invoke_fake(json.dumps(make_plan()).encode())
        self.assertEqual(self.output.read_text(), "unrelated race winner")

    def test_head_change_is_rejected_before_publication(self):
        initial = self.git("rev-parse", "HEAD")
        with patch.object(plan, "commit", side_effect=[initial, "f" * 40]), self.assertRaises(ValueError):
            self.invoke_fake(json.dumps(make_plan()).encode())
        self.assertFalse(self.output.exists())


CANDIDATE = os.environ.get("URMARE_TEST_BINARY")


@unittest.skipUnless(CANDIDATE, "set URMARE_TEST_BINARY to the locally built candidate executable")
class CandidateTests(Sandbox):
    def setUp(self):
        super().setUp()
        self.env["URMARE_BINARY"] = str(Path(CANDIDATE).resolve())
        version = subprocess.run([self.env["URMARE_BINARY"], "--version"], capture_output=True,
                                 text=True, check=True).stdout.strip().split()[1]
        self.env["URMARE_VERSION"] = version
        self.base = self.init()
        self.env["URMARE_BASE"] = self.base

    def action(self, success=True):
        result = subprocess.run([sys.executable, str(REPO / "scripts/ci/plan.py")], env=self.env,
                                capture_output=True, text=True, check=False)
        if success:
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(self.output.read_text())
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.output.exists())
        return result

    def test_committed_pytest_aware_changes_and_consumer(self):
        (self.root / "src/service.py").write_text("VALUE = 2\n")
        self.commit()
        before = self.git("status", "--porcelain", "--untracked-files=all")
        result = self.action()
        direct = subprocess.run(plan.cli_command(self.env["URMARE_BINARY"], self.root, self.base),
                                capture_output=True, check=True)
        self.assertEqual(self.output.read_bytes(), direct.stdout)
        self.assertEqual(result["validation"]["mode"], "selective")
        self.assertIn("tests/test_service.py", result["affected_tests"])
        with patch.object(consumer.subprocess, "run", return_value=subprocess.CompletedProcess([], 3)) as run:
            self.assertEqual(consumer.run_plan(self.output, self.root), 3)
            self.assertIn("./tests/test_service.py", run.call_args.args[0])
        self.assertEqual(self.git("status", "--porcelain", "--untracked-files=all"), before)

    def test_unchanged_none(self):
        self.assertEqual(self.action()["validation"], dict(mode="none", steps=[]))

    def test_local_action_does_not_create_bytecode_in_analyzed_checkout(self):
        local = self.root / "local-action"
        shutil.copytree(REPO / "scripts/ci", local / "scripts/ci", ignore=shutil.ignore_patterns("__pycache__"))
        shutil.copytree(REPO / "examples/ci", local / "examples/ci", ignore=shutil.ignore_patterns("__pycache__"))
        self.commit()
        self.env["URMARE_BASE"] = self.git("rev-parse", "HEAD")
        result = subprocess.run([sys.executable, str(local / "scripts/ci/plan.py")],
                                env=self.env, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(list(self.root.rglob("__pycache__")))
        self.assertEqual(self.git("status", "--porcelain", "--untracked-files=all"), "")

    def test_uses_merge_base_not_base_tree_and_keeps_exclusions(self):
        # A base-branch-only modification must not enter the PR's changed list.
        branch = self.git("branch", "--show-current")
        self.git("checkout", "--quiet", "-b", "base-advanced")
        (self.root / "src/base_only.py").write_text("VALUE = 3\n")
        self.commit()
        self.env["URMARE_BASE"] = self.git("rev-parse", "HEAD")
        self.git("checkout", "--quiet", branch)
        (self.root / "src/service.py").write_text("VALUE = 2\n")
        self.commit()
        result = self.action()
        self.assertEqual(result["changed"], ["src/service.py"])

    def test_excluded_changes_and_option_like_test_filename(self):
        (self.root / "pyproject.toml").write_text(
            '[tool.urmare]\nexclude = ["src/ignored.py"]\ntest-roots = ["tests"]\n')
        (self.root / "src/ignored.py").write_text("VALUE = 1\n")
        self.commit()
        self.env["URMARE_BASE"] = self.git("rev-parse", "HEAD")
        (self.root / "src/ignored.py").write_text("VALUE = 2\n")
        self.commit()
        self.assertEqual(self.action()["validation"]["mode"], "none")
        self.env["URMARE_BASE"] = self.git("rev-parse", "HEAD")
        # Configured test roots allow an option-like filename, still kept as one argument.
        (self.root / "tests/-test file ;$`&.py").write_text("from service import VALUE\n")
        self.commit()
        self.output = self.temp / "option-like.json"
        self.env["URMARE_PLAN_PATH"] = str(self.output)
        result = self.action()
        self.assertIn("tests/-test file ;$`&.py", result["affected_tests"])

    def test_deleted_and_renamed_identities(self):
        self.git("mv", "src/service.py", "src/renamed.py")
        self.commit()
        result = self.action()
        self.assertIn("src/service.py", result["changed"])
        self.assertIn("src/renamed.py", result["changed"])
        self.assertIn("tests/test_service.py", result["affected_tests"])
        # Fresh invocation with deleted source and a new output path.
        (self.root / "src/renamed.py").unlink()
        self.commit()
        self.env["URMARE_PLAN_PATH"] = str(self.temp / "deleted.json")
        self.output = Path(self.env["URMARE_PLAN_PATH"])
        self.assertIn("tests/test_service.py", self.action()["affected_tests"])

    def test_all_conservative_fallbacks(self):
        cases = [("pyproject.toml", "[tool.urmare]\n", "configuration_changed"),
                 ("src/dynamic.py", "import importlib\nimportlib.import_module(name)\n", "unresolved_dynamic_import"),
                 ("src/exports.py", "__all__ = compute()\n", "unsupported_exports"),
                 ("tests/test_lookup.py", "def test_lookup(request):\n    request.getfixturevalue(name)\n", "unsupported_pytest_relationship")]
        for index, (path, source, reason) in enumerate(cases):
            with self.subTest(reason=reason):
                self.env["URMARE_BASE"] = self.git("rev-parse", "HEAD")
                (self.root / path).write_text(source)
                self.commit()
                self.output = self.temp / f"fallback-{index}.json"
                self.env["URMARE_PLAN_PATH"] = str(self.output)
                result = self.action()
                self.assertEqual(result["validation"]["mode"], "full")
                self.assertEqual(result["full_validation"]["reason"], reason)
                self.assertEqual(result["validation"]["steps"][0]["args"], [])

    def test_invalid_unavailable_and_shallow_bases_fail(self):
        for base in ("", "--help", "missing-ref", "HEAD^{not-a-type}", "a" * 40):
            self.env["URMARE_BASE"] = base
            result = self.action(success=False)
            if base == "missing-ref":
                self.assertEqual(result.returncode, 3)
        (self.root / "src/service.py").write_text("VALUE = 2\n")
        self.commit()
        shallow = self.temp / "shallow"
        subprocess.run(["git", "clone", "--quiet", "--depth=1", self.root.as_uri(), str(shallow)], check=True)
        self.env["URMARE_ROOT"] = str(shallow)
        self.env["URMARE_BASE"] = "HEAD"
        self.assertIn("fetch-depth", self.action(success=False).stderr)

    def test_fresh_checkout_and_unrelated_history(self):
        (self.root / "src/service.py").write_text("VALUE = 2\n")
        self.commit()
        clone = self.temp / "fresh checkout"
        subprocess.run(["git", "clone", "--quiet", self.root.as_uri(), str(clone)], check=True)
        self.env["URMARE_ROOT"] = str(clone)
        self.assertEqual(self.action()["validation"]["mode"], "selective")
        self.output = self.temp / "unrelated.json"
        self.env["URMARE_PLAN_PATH"] = str(self.output)
        self.git("checkout", "--orphan", "unrelated", root=clone)
        self.git("-c", "user.name=CI", "-c", "user.email=ci@example.invalid",
                 "commit", "--quiet", "-m", "unrelated", root=clone)
        self.action(success=False)


if __name__ == "__main__":
    unittest.main()
