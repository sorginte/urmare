#!/usr/bin/env python3
"""Vendor this file into .github/scripts/urmare_pytest.py in a uv project.

This is an example consumer, not an Urmare execution command. Edit the literal
subprocess prefix in run_plan to use your repository's established environment.
The planning Action imports only load_plan; it never calls run_plan.
"""

import json
from pathlib import Path, PurePosixPath, PureWindowsPath
import subprocess
import sys


class PlanError(ValueError):
    """A plan cannot safely be consumed by this pytest-only integration."""


def require(condition, message):
    if not condition:
        raise PlanError(message)


def fields(value, required, optional=()):
    require(isinstance(value, dict), "expected a JSON object")
    require(set(required) <= value.keys() <= set(required) | set(optional),
            "missing or unsupported plan fields")


def repository_path(value):
    require(isinstance(value, str) and bool(value), "expected a nonempty path string")
    require(not any(ord(c) < 32 for c in value), "control character in path")
    require("\\" not in value and ":" not in value, "nonportable path or pytest node ID")
    require(not PurePosixPath(value).is_absolute() and not PureWindowsPath(value).drive,
            "absolute repository path")
    require(all(part not in ("", ".", "..") for part in value.split("/")),
            "repository path must be normalized and cannot escape root")
    return value


def paths(value):
    require(isinstance(value, list), "expected a path array")
    result = [repository_path(item) for item in value]
    require(len(set(result)) == len(result), "duplicate paths")
    return result


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def load_plan(path, root):
    """Validate the v1 DTO and mode/step consistency before any execution."""
    root = Path(root).resolve(strict=True)
    with Path(path).open(encoding="utf-8") as handle:
        plan = json.load(handle, object_pairs_hook=unique_object)
    fields(plan, ("schema_version", "changed", "directly_affected", "transitively_affected",
                  "affected_tests", "validation", "attributions"), ("full_validation",))
    require(type(plan["schema_version"]) is int and plan["schema_version"] == 1,
            "unsupported schema_version (expected 1)")
    for name in ("changed", "directly_affected", "transitively_affected", "affected_tests"):
        paths(plan[name])
    for target in plan["affected_tests"]:
        resolved = (root / target).resolve(strict=True)
        require(root in resolved.parents and resolved.is_file() and target.endswith(".py"),
                "test target must be an existing Python file inside root")
    require(isinstance(plan["attributions"], list), "expected attribution array")
    for attribution in plan["attributions"]:
        fields(attribution, ("affected", "caused_by"))
        repository_path(attribution["affected"])
        paths(attribution["caused_by"])
    validation = plan["validation"]
    fields(validation, ("mode", "steps"))
    mode, steps = validation["mode"], validation["steps"]
    require(mode in ("selective", "none", "full"), "unknown validation mode")
    require(isinstance(steps, list), "expected step array")
    for step in steps:
        fields(step, ("kind", "program", "args"))
        require(step["kind"] == "pytest" and step["program"] == "pytest",
                "only pytest steps are supported")
        paths(step["args"])
    full = plan.get("full_validation")
    if mode == "full":
        fields(full, ("required", "reason", "configuration_paths"))
        require(full["required"] is True and full["reason"] in (
            "configuration_changed", "unresolved_dynamic_import",
            "unsupported_pytest_relationship", "unsupported_exports"),
            "unknown or invalid full-validation reason")
        require(bool(paths(full["configuration_paths"])), "missing full-validation paths")
        require(len(steps) == 1 and steps[0]["args"] == [],
                "full mode requires one targetless pytest step")
    else:
        require("full_validation" not in plan, "full-validation metadata in non-full mode")
        if mode == "none":
            require(not steps and not plan["affected_tests"], "inconsistent none plan")
        else:
            require(bool(steps) and all(step["args"] for step in steps),
                    "selective mode requires nonempty pytest steps")
            selected = [target for step in steps for target in step["args"]]
            require(selected == plan["affected_tests"],
                    "selective steps must match every affected test in order")
    return plan


def run_plan(path, root):
    root = Path(root).resolve(strict=True)
    plan = load_plan(path, root)  # Validate every step before executing the first.
    if plan["validation"]["mode"] == "none":
        print("Urmare: none; skipping pytest explicitly.", flush=True)
        return 0
    print("Urmare: " + plan["validation"]["mode"] + "; running required pytest validation.",
          flush=True)
    status = 0
    for step in plan["validation"]["steps"]:
        # Never execute the JSON program field. '--' and './' prevent option parsing.
        targets = ["--"] + ["./" + target for target in step["args"]] if step["args"] else []
        result = subprocess.run(["uv", "run", "pytest", *targets], cwd=root, check=False)
        if result.returncode:
            status = result.returncode if result.returncode > 0 else 1
    return status


if __name__ == "__main__":
    try:
        require(len(sys.argv) == 3, "usage: urmare_pytest.py PLAN_JSON REPOSITORY_ROOT")
        sys.exit(run_plan(sys.argv[1], sys.argv[2]))
    except (PlanError, OSError, ValueError) as error:
        print(f"Urmare plan consumption failed: {str(error)!r}", file=sys.stderr)
        sys.exit(1)
