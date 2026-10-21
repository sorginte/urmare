#!/usr/bin/env python3
"""Planning-only GitHub Action adapter. Analysis stays entirely in the CLI."""

import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import uuid

sys.dont_write_bytecode = True  # Local uses: ./ must not create __pycache__ in the checkout.

from install import install, version_string

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "examples" / "ci"))
from run_pytest import load_plan  # noqa: E402


def clean(value, label):
    if any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError(f"{label} cannot contain control characters")
    return value


def inputs(env):
    workspace = Path(env["GITHUB_WORKSPACE"]).resolve(strict=True)
    root_input = clean(env.get("URMARE_ROOT", ""), "root")
    root = (workspace / root_input).resolve(strict=True) if root_input else workspace
    if not root.is_dir():
        raise ValueError("root must be an existing Git repository directory")
    base = clean(env.get("URMARE_BASE", ""), "base")
    if not base.strip() or base.startswith("-"):
        raise ValueError("base is required and cannot start with '-'; use the PR recorded base SHA")
    version = version_string(env.get("URMARE_VERSION", ""))
    temp = Path(env["RUNNER_TEMP"]).resolve(strict=True)
    if root == temp or root in temp.parents:
        raise ValueError("RUNNER_TEMP must be outside the analyzed repository")
    path_input = clean(env.get("URMARE_PLAN_PATH", ""), "plan-path")
    output = Path(path_input) if path_input else temp / f"urmare-plan-{uuid.uuid4().hex}.json"
    if not output.is_absolute():
        raise ValueError("plan-path must be absolute and outside the analyzed repository")
    if output.is_symlink() or output.exists():
        raise ValueError("plan-path already exists; choose a new path (no files are overwritten)")
    output = output.parent.resolve(strict=True) / output.name
    if root in output.parents:
        raise ValueError("plan-path must be outside the analyzed repository; use RUNNER_TEMP")
    binary = clean(env.get("URMARE_BINARY", ""), "binary")
    return workspace, root, base, version, temp, output, binary


def git(root, *args):
    result = subprocess.run(["git", "-C", str(root), *args], capture_output=True,
                            text=True, check=False)
    if result.returncode:
        raise ValueError("Git history unavailable: " + result.stderr.strip()
                         + "; check base and use checkout fetch-depth: 0")
    return result.stdout.strip()


def commit(root):
    value = git(root, "rev-parse", "--verify", "HEAD^{commit}")
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", value):
        raise ValueError("Git did not report a valid analyzed commit")
    return value


def cli_command(binary, root, base):
    return [str(binary), "--root", str(root), "plan", "--git-diff", base, "--json"]


def code(value):
    # Encode punctuation too: inline HTML alone does not suppress Markdown links.
    escaped = "".join(c if c.isalnum() or c == " " else f"&#{ord(c)};" for c in str(value))
    return "<code>" + escaped + "</code>"


def outputs(path, values):
    with Path(path).open("a", encoding="utf-8", newline="\n") as handle:
        for name, value in values.items():
            clean(str(value), "workflow output")
            delimiter = "urmare_" + uuid.uuid4().hex
            handle.write(f"{name}<<{delimiter}\n{value}\n{delimiter}\n")


def main(env=None):
    env = os.environ if env is None else env
    workspace, root, base, version, temp, output, override = inputs(env)
    analyzed = commit(root)
    # Reject shallow history even when Git could return an incomplete merge base.
    if git(root, "rev-parse", "--is-shallow-repository") != "false":
        raise ValueError("shallow history cannot establish a complete comparison; use fetch-depth: 0")
    with tempfile.TemporaryDirectory(prefix="urmare-action-", dir=temp) as owned:
        binary = install(version, override, Path(owned), workspace)
        # The temporary sibling and final path share a filesystem for atomic publication.
        with tempfile.TemporaryDirectory(prefix=".urmare-plan-", dir=output.parent) as staging:
            draft = Path(staging) / "plan.json"
            with draft.open("xb") as stdout:
                result = subprocess.run(cli_command(binary, root, base), stdout=stdout,
                                        stderr=subprocess.PIPE, check=False)
                stdout.flush()
                os.fsync(stdout.fileno())
            if result.stderr:
                # repr prevents repository-controlled text from becoming runner commands.
                print("Urmare diagnostics: " + repr(result.stderr.decode("utf-8", errors="replace")),
                      file=sys.stderr)
            if result.returncode:
                print("Planning failed; verify the local base and complete Git history (fetch-depth: 0).",
                      file=sys.stderr)
                return result.returncode if result.returncode > 0 else 1
            plan = load_plan(draft, root)
            if commit(root) != analyzed:
                raise ValueError("HEAD changed during planning; recompute on a fixed checkout")
            if env.get("GITHUB_STEP_SUMMARY"):
                with Path(env["GITHUB_STEP_SUMMARY"]).open("a", encoding="utf-8") as handle:
                    handle.write("### Urmare validation plan\n\n"
                                 f"Analyzed commit: {code(analyzed)}  \nComparison base: {code(base)}  \n"
                                 f"Mode: {code(plan['validation']['mode'])}  \n"
                                 f"Changed Python files: {len(plan['changed'])}; "
                                 f"affected test files: {len(plan['affected_tests'])}.\n\n")
                    if "full_validation" in plan:
                        handle.write("Full-validation reason: " + code(plan["full_validation"]["reason"]) + "\n\n")
                    handle.write("Complete JSON (only valid after this step succeeds): " + code(output)
                                 + ". Preserve with actions/upload-artifact in the consuming workflow.\n")
            outputs(env["GITHUB_OUTPUT"], {"mode": plan["validation"]["mode"], "plan-path": output,
                                          "affected-test-count": len(plan["affected_tests"])})
            # Last operation: atomic no-clobber publication. No file exists on earlier errors.
            os.link(draft, output)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Urmare planning Action failed: {str(error)!r}", file=sys.stderr)
        sys.exit(1)
