"""Prepare and check the purpose-built fixture for the composite Action CI smoke."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts/ci"))
sys.path.insert(0, str(REPO / "examples/ci"))
from plan import outputs
from run_pytest import run_plan


def prepare():
    root = Path(tempfile.mkdtemp(prefix="urmare-ci-fixture-", dir=os.environ["RUNNER_TEMP"]))
    shutil.copytree(REPO / "fixtures/python-projects/ci-project", root, dirs_exist_ok=True)
    def git(*args):
        return subprocess.run(["git", "-C", str(root), *args], check=True,
                              capture_output=True, text=True).stdout.strip()
    git("init", "--quiet")
    git("config", "user.name", "Urmare CI")
    git("config", "user.email", "ci@example.invalid")
    git("add", ".")
    git("commit", "--quiet", "-m", "base")
    base = git("rev-parse", "HEAD")
    (root / "src/service.py").write_text("VALUE = 2\n", encoding="utf-8")
    git("add", ".")
    git("commit", "--quiet", "-m", "PR change")
    binary = (REPO / "target/release" / ("urmare.exe" if os.name == "nt" else "urmare")).resolve()
    version = subprocess.run([str(binary), "--version"], check=True, capture_output=True,
                             text=True).stdout.strip().split()[1]
    outputs(os.environ["GITHUB_OUTPUT"], {"root": root, "base": base,
                                         "binary": binary, "version": version})


def check():
    path = Path(os.environ["PLAN_PATH"])
    root = Path(os.environ["FIXTURE_ROOT"])
    value = json.loads(path.read_text(encoding="utf-8"))
    if value["validation"]["mode"] != "selective" or "tests/test_service.py" not in value["affected_tests"]:
        raise ValueError("fixture did not produce expected pytest-aware selective plan")
    with patch("run_pytest.subprocess.run", return_value=subprocess.CompletedProcess([], 0)) as run:
        if run_plan(path, root) != 0 or not run.called:
            raise ValueError("consumer did not execute the plan")
        commands = [call.args[0] for call in run.call_args_list]
    log = Path(os.environ["RUNNER_TEMP"]) / "urmare-ci-consumer.json"
    log.write_text(json.dumps(commands, indent=2) + "\n", encoding="utf-8")
    if subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                      capture_output=True, check=True).stdout:
        raise ValueError("planning modified the fixture checkout")


if __name__ == "__main__":
    if sys.argv[1:] == ["prepare"]:
        prepare()
    elif sys.argv[1:] == ["check"]:
        check()
    else:
        raise SystemExit("usage: smoke.py prepare|check")
