use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_urmare")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/python-projects")
        .join(name)
        .canonicalize()
        .expect("canonical fixture")
}

struct McpProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl McpProcess {
    fn start(root: &Path) -> (Self, Value) {
        Self::start_with_path(root, None)
    }

    fn start_with_path(root: &Path, path: Option<OsString>) -> (Self, Value) {
        let mut command = Command::new(binary());
        command
            .args(["--root", root.to_str().expect("UTF-8 root"), "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = path {
            command.env("PATH", path);
        }
        let mut child = command.spawn().expect("start MCP server");
        let stdin = child.stdin.take().expect("MCP stdin");
        let stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));
        let mut server = Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout,
        };
        server.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "urmare-tests", "version": "1"}
            }
        }));
        let initialized = server.read();
        (server, initialized)
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("open MCP stdin");
        serde_json::to_writer(&mut *stdin, message).expect("write protocol JSON");
        stdin.write_all(b"\n").expect("write protocol delimiter");
        stdin.flush().expect("flush protocol request");
    }

    fn send_raw(&mut self, message: &str) {
        let stdin = self.stdin.as_mut().expect("open MCP stdin");
        stdin
            .write_all(message.as_bytes())
            .expect("write raw input");
        stdin.write_all(b"\n").expect("write raw delimiter");
        stdin.flush().expect("flush raw input");
    }

    fn notify_initialized(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
    }

    fn request(&mut self, id: Value, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        self.read()
    }

    fn call(&mut self, id: Value, name: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        let bytes = self.stdout.read_line(&mut line).expect("read MCP response");
        assert_ne!(bytes, 0, "MCP server closed before sending a response");
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("stdout was not valid JSON-RPC: {error}; line={line:?}"))
    }

    fn finish(mut self) -> (ExitStatus, String, String) {
        drop(self.stdin.take());
        let mut remaining_stdout = String::new();
        self.stdout
            .read_to_string(&mut remaining_stdout)
            .expect("remaining MCP stdout");
        let mut child = self.child.take().expect("MCP child");
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("MCP stderr")
            .read_to_string(&mut stderr)
            .expect("read MCP stderr");
        let status = child.wait().expect("MCP exit status");
        (status, remaining_stdout, stderr)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn initialization_discovery_and_cli_contract_parity() {
    let root = fixture("src-layout");
    let (mut server, initialized) = McpProcess::start(&root);
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "urmare");
    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(initialized["result"]["capabilities"]["tools"].is_object());
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .expect("instructions")
            .starts_with("Run `validation_plan` after Python edits.")
    );

    server.notify_initialized();
    let listed = server.request(json!("tools"), "tools/list", json!({}));
    assert_eq!(listed["id"], "tools");
    let tools = listed["result"]["tools"].as_array().expect("tool list");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], "validation_plan");
    assert_eq!(tools[1]["name"], "explain_impact");
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["annotations"]["destructiveHint"], false);
        assert_eq!(tool["annotations"]["openWorldHint"], false);
        assert!(tool["inputSchema"].is_object());
        assert!(tool["outputSchema"].is_object());
    }

    for (id, files) in [
        (2, vec!["src/payments/stripe.py"]),
        (3, vec!["src/payments/service.py", "src/payments/stripe.py"]),
        (4, vec!["src/cycles/a.py"]),
    ] {
        let response = server.call(
            json!(id),
            "validation_plan",
            json!({"source":{"kind":"files","files":files}}),
        );
        let structured = successful_structured(&response);
        let mut cli_arguments = vec!["plan"];
        cli_arguments.extend(files);
        cli_arguments.push("--json");
        assert_eq!(structured, cli_json(&root, &cli_arguments));
    }

    let explanation = server.call(
        json!(5),
        "explain_impact",
        json!({
            "changed_file":"src/payments/stripe.py",
            "affected_file":"tests/api/test_checkout.py",
            "context":{"kind":"current"}
        }),
    );
    assert_eq!(
        successful_structured(&explanation),
        cli_json(
            &root,
            &[
                "why",
                "src/payments/stripe.py",
                "tests/api/test_checkout.py",
                "--json"
            ]
        )
    );

    let escaped = server.call(
        json!(6),
        "validation_plan",
        json!({"source":{"kind":"files","files":["../outside.py"]}}),
    );
    assert_eq!(escaped["result"]["isError"], true);
    let request_root = server.call(
        json!(7),
        "validation_plan",
        json!({"source":{"kind":"changed"},"root":"/tmp/not-allowed"}),
    );
    assert_eq!(request_root["error"]["code"], -32602);
    let conflicting = server.call(
        json!(8),
        "validation_plan",
        json!({"source":{"kind":"changed","files":["src/payments/stripe.py"]}}),
    );
    assert_eq!(conflicting["error"]["code"], -32602);

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty(), "unexpected stdout after EOF: {stdout}");
    assert!(stderr.is_empty(), "unexpected MCP diagnostics: {stderr}");
}

#[test]
fn coverage_evidence_and_fallbacks_match_cli_after_main_merge() {
    let repository = initialized_git_repository();
    let root = repository.path();
    for (path, source) in [
        ("src/pkg/source.py", "VALUE = 1\n"),
        (
            "src/pkg/__init__.py",
            "from .source import VALUE as PublicValue\n",
        ),
        ("src/consumer.py", "from pkg import PublicValue\n"),
        (
            "src/loader.py",
            "import importlib as il\nil.import_module(\"pkg.source\")\n",
        ),
        ("src/pkg/plugin_dependency.py", "VALUE = 1\n"),
        (
            "src/pkg/plugin.py",
            "import pytest\nfrom . import plugin_dependency\n@pytest.fixture\ndef plugin_value():\n    return plugin_dependency.VALUE\n",
        ),
        ("conftest.py", "pytest_plugins = []\n"),
        ("tests/test_consumer.py", "import consumer\n"),
        (
            "tests/test_fixture.py",
            "def test_value(plugin_value):\n    assert plugin_value\n",
        ),
        (
            "src/computed.py",
            "import importlib\nimportlib.import_module(module_name)\n",
        ),
        ("src/exports.py", "__all__ = exported_names\n"),
        (
            "tests/test_lookup.py",
            "def test_lookup(request):\n    request.getfixturevalue(fixture_name)\n",
        ),
    ] {
        write(root, path, source);
    }
    git(root, &["add", "."]);
    commit(root, "coverage fixture");
    let (mut server, _) = McpProcess::start(root);
    server.notify_initialized();

    for (id, changed, affected, evidence_kind) in [
        (
            2,
            "conftest.py",
            "tests/test_fixture.py",
            "pytest_configuration",
        ),
        (3, "src/pkg/source.py", "src/consumer.py", "re_export"),
        (4, "src/pkg/source.py", "src/loader.py", "dynamic_import"),
    ] {
        let plan = successful_structured(&server.call(
            json!(format!("plan-{id}")),
            "validation_plan",
            json!({"source":{"kind":"files","files":[changed]}}),
        ));
        assert_eq!(plan, cli_json(root, &["plan", changed, "--json"]));
        let explanation = successful_structured(&server.call(
            json!(id),
            "explain_impact",
            json!({"changed_file":changed,"affected_file":affected,"context":{"kind":"current"}}),
        ));
        assert_eq!(
            explanation,
            cli_json(root, &["why", changed, affected, "--json"])
        );
        assert!(
            explanation["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .any(|step| {
                    step["evidence"].as_array().is_some_and(|evidence| {
                        evidence.iter().any(|item| item["kind"] == evidence_kind)
                    })
                })
        );
    }

    // Keep the server alive while plugin declarations change. Dependents are
    // not edited: the refreshed index must attach them to the local plugin.
    let before = successful_structured(&server.call(
        json!(5),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/pkg/plugin_dependency.py"]}}),
    ));
    assert_eq!(before["validation"]["mode"], "none");
    write(root, "conftest.py", "pytest_plugins = [\"pkg.plugin\"]\n");
    let after = successful_structured(&server.call(
        json!(6),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/pkg/plugin_dependency.py"]}}),
    ));
    assert!(
        after["affected_tests"]
            .as_array()
            .expect("tests")
            .contains(&json!("tests/test_fixture.py"))
    );
    assert_eq!(
        after,
        cli_json(root, &["plan", "src/pkg/plugin_dependency.py", "--json"])
    );
    let clean = successful_structured(&server.call(
        json!(7),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/pkg/plugin_dependency.py"]}}),
    ));
    assert_eq!(clean, after);

    for (id, path, reason) in [
        (8, "src/computed.py", "unresolved_dynamic_import"),
        (9, "src/exports.py", "unsupported_exports"),
        (
            10,
            "tests/test_lookup.py",
            "unsupported_pytest_relationship",
        ),
    ] {
        let plan = successful_structured(&server.call(
            json!(id),
            "validation_plan",
            json!({"source":{"kind":"files","files":[path]}}),
        ));
        assert_eq!(plan["validation"]["mode"], "full");
        assert_eq!(plan["full_validation"]["reason"], reason);
        assert_eq!(plan, cli_json(root, &["plan", path, "--json"]));
    }
    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn protocol_errors_and_tool_errors_do_not_terminate_the_server() {
    let root = fixture("src-layout");
    let (mut server, _) = McpProcess::start(&root);
    server.notify_initialized();

    server.send_raw("{not JSON");
    let malformed = server.read();
    assert_eq!(malformed["error"]["code"], -32700);

    let unknown_method = server.request(json!("unknown"), "urmare/unknown", json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601);
    let unknown_tool = server.call(json!(3), "unknown_tool", json!({}));
    assert_eq!(unknown_tool["error"]["code"], -32601);
    let invalid = server.call(
        json!(4),
        "validation_plan",
        json!({"source":{"kind":"files","files":[]}}),
    );
    assert_eq!(invalid["error"]["code"], -32602);

    let analysis_failure = server.call(
        json!(5),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/missing.py"]}}),
    );
    assert_eq!(analysis_failure["result"]["isError"], true);
    assert!(
        analysis_failure["result"]["content"][0]["text"]
            .as_str()
            .expect("error text")
            .contains("does not exist")
    );

    server.send(&json!({
        "jsonrpc":"2.0",
        "method":"notifications/cancelled",
        "params":{"requestId":999,"reason":"test cancellation"}
    }));
    let ping = server.request(json!(6), "ping", json!({}));
    assert_eq!(
        ping["id"], 6,
        "notification unexpectedly produced a response"
    );

    let recovered = server.call(
        json!(7),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/payments/stripe.py"]}}),
    );
    assert_eq!(successful_structured(&recovered)["schema_version"], 1);

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.contains("MCP protocol input error:"));
    assert!(!stderr.contains("{not JSON"), "raw input leaked to stderr");
}

#[test]
fn active_cancellation_keeps_the_server_usable() {
    let repository = tempdir().expect("cancellation repository");
    write(repository.path(), "pkg/__init__.py", "");
    for index in 0..500 {
        let contents = if index == 0 {
            "VALUE = 0\n".to_owned()
        } else {
            format!("from . import module_{:04}\n", index - 1)
        };
        write(
            repository.path(),
            &format!("pkg/module_{index:04}.py"),
            &contents,
        );
    }
    write(
        repository.path(),
        "tests/test_tail.py",
        "from pkg import module_0499\n",
    );
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);
    commit(repository.path(), "base");

    let (mut server, _) = McpProcess::start(repository.path());
    server.notify_initialized();
    let call = |id| {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"validation_plan","arguments":{"source":{
                "kind":"files","files":["pkg/module_0000.py"]
            }}
        }})
    };
    server.send(&call(2));
    server.send(&call(3));
    server.send(&json!({
        "jsonrpc":"2.0",
        "method":"notifications/cancelled",
        "params":{"requestId":3,"reason":"integration test"}
    }));
    server.send(&json!({"jsonrpc":"2.0","id":4,"method":"ping"}));
    server.send(&call(5));

    let mut responses = BTreeMap::new();
    while ![2, 4, 5].iter().all(|id| responses.contains_key(id)) {
        let response = server.read();
        let id = response["id"].as_i64().expect("numeric response ID");
        responses.insert(id, response);
    }
    assert_eq!(successful_structured(&responses[&2])["schema_version"], 1);
    assert_eq!(responses[&4]["result"], json!({}));
    assert_eq!(successful_structured(&responses[&5])["schema_version"], 1);
    if let Some(cancelled) = responses.get(&3) {
        assert_eq!(cancelled["result"]["isError"], true);
        assert!(
            cancelled["result"]["content"][0]["text"]
                .as_str()
                .expect("cancellation text")
                .contains("cancelled")
        );
    }

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn long_running_server_observes_edits_reuses_clean_index_and_never_executes_pytest() {
    let repository = tempdir().expect("temporary repository");
    copy_tree(&fixture("src-layout"), repository.path());
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);
    commit(repository.path(), "base");

    let fake_bin = tempdir().expect("fake executable directory");
    let marker = fake_bin.path().join("pytest-was-run");
    install_fake_pytest(fake_bin.path(), &marker);
    let path = std::env::join_paths(std::iter::once(fake_bin.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("test PATH");

    let (mut server, _) = McpProcess::start_with_path(repository.path(), Some(path));
    server.notify_initialized();
    let first = server.call(
        json!(2),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/payments/stripe.py"]}}),
    );
    assert_eq!(
        successful_structured(&first)["affected_tests"]
            .as_array()
            .expect("test targets")
            .len(),
        2
    );

    fs::write(
        repository.path().join("tests/helpers_test.py"),
        "from payments import stripe\n\ndef test_helper():\n    assert stripe is not None\n",
    )
    .expect("external edit");
    let contents_before_calls = repository_snapshot(repository.path());
    let status_before_calls = git_output(repository.path(), &["status", "--porcelain=v1", "-z"]);

    let second = server.call(
        json!(3),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/payments/stripe.py"]}}),
    );
    let second_structured = successful_structured(&second);
    assert!(
        second_structured["affected_tests"]
            .as_array()
            .expect("test targets")
            .contains(&json!("tests/helpers_test.py"))
    );
    let third = server.call(
        json!(4),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/payments/stripe.py"]}}),
    );
    assert_eq!(successful_structured(&third), second_structured);
    assert_eq!(
        second_structured,
        cli_json(
            repository.path(),
            &["plan", "src/payments/stripe.py", "--json"]
        )
    );

    // Back-to-back calls are accepted before either response is consumed;
    // the in-process analysis gate serializes their index refreshes.
    server.send(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
        "name":"validation_plan","arguments":{"source":{"kind":"files","files":["src/payments/stripe.py"]}}
    }}));
    server.send(&json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{
        "name":"validation_plan","arguments":{"source":{"kind":"files","files":["src/payments/stripe.py"]}}
    }}));
    let concurrent_a = server.read();
    let concurrent_b = server.read();
    assert_eq!(successful_structured(&concurrent_a), second_structured);
    assert_eq!(successful_structured(&concurrent_b), second_structured);

    assert_eq!(
        repository_snapshot(repository.path()),
        contents_before_calls
    );
    assert_eq!(
        git_output(repository.path(), &["status", "--porcelain=v1", "-z"]),
        status_before_calls
    );
    assert!(!marker.exists(), "Urmare executed the fake pytest program");

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn git_modes_preserve_deleted_and_renamed_identities_and_configuration_fallback() {
    let repository = initialized_git_repository();
    let base = git_output(repository.path(), &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let (mut server, _) = McpProcess::start(repository.path());
    server.notify_initialized();

    let clean = server.call(
        json!(2),
        "validation_plan",
        json!({"source":{"kind":"changed"}}),
    );
    assert_eq!(successful_structured(&clean)["validation"]["mode"], "none");

    // Seed the persistent baseline, then mutate the checkout outside the server.
    let _ = server.call(
        json!(3),
        "validation_plan",
        json!({"source":{"kind":"files","files":["src/pkg/deleted.py"]}}),
    );
    fs::remove_file(repository.path().join("src/pkg/deleted.py")).expect("deleted module");
    git(
        repository.path(),
        &["mv", "src/pkg/old.py", "src/pkg/renamed.py"],
    );
    write(
        repository.path(),
        "tests/test_untracked.py",
        "from pkg import kept\n",
    );
    let status_before = git_output(repository.path(), &["status", "--porcelain=v1", "-z"]);

    let changed = server.call(
        json!(4),
        "validation_plan",
        json!({"source":{"kind":"changed"}}),
    );
    assert_eq!(
        successful_structured(&changed),
        cli_json(repository.path(), &["plan", "--changed", "--json"])
    );
    let deleted_why = server.call(
        json!(5),
        "explain_impact",
        json!({
            "changed_file":"src/pkg/deleted.py",
            "affected_file":"tests/test_deleted.py",
            "context":{"kind":"changed"}
        }),
    );
    assert_eq!(
        successful_structured(&deleted_why),
        cli_json(
            repository.path(),
            &[
                "why",
                "src/pkg/deleted.py",
                "tests/test_deleted.py",
                "--changed",
                "--json"
            ]
        )
    );
    assert_eq!(
        git_output(repository.path(), &["status", "--porcelain=v1", "-z"]),
        status_before
    );

    git(repository.path(), &["add", "-A"]);
    commit(repository.path(), "rename and delete");
    let diff = server.call(
        json!(6),
        "validation_plan",
        json!({"source":{"kind":"git_diff","base":base}}),
    );
    assert_eq!(
        successful_structured(&diff),
        cli_json(repository.path(), &["plan", "--git-diff", &base, "--json"])
    );
    let renamed_why = server.call(
        json!(7),
        "explain_impact",
        json!({
            "changed_file":"src/pkg/old.py",
            "affected_file":"tests/test_old.py",
            "context":{"kind":"git_diff","base":base}
        }),
    );
    assert_eq!(
        successful_structured(&renamed_why),
        cli_json(
            repository.path(),
            &[
                "why",
                "src/pkg/old.py",
                "tests/test_old.py",
                "--git-diff",
                &base,
                "--json"
            ]
        )
    );

    fs::write(
        repository.path().join("pyproject.toml"),
        "[tool.urmare]\nsource-roots = [\"src\"]\ntest-roots = [\"tests\"]\n# changed\n",
    )
    .expect("configuration edit");
    let full = server.call(
        json!(8),
        "validation_plan",
        json!({"source":{"kind":"changed"}}),
    );
    let full = successful_structured(&full);
    assert_eq!(full["validation"]["mode"], "full");
    assert_eq!(full["full_validation"]["reason"], "configuration_changed");
    let invalid_base = server.call(
        json!(9),
        "validation_plan",
        json!({"source":{"kind":"git_diff","base":"refs/heads/does-not-exist"}}),
    );
    assert_eq!(invalid_base["result"]["isError"], true);

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

#[test]
fn root_binding_and_security_fail_closed() {
    let missing = Command::new(binary())
        .arg("mcp")
        .output()
        .expect("missing-root invocation");
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());

    let relative = Command::new(binary())
        .args(["--root", ".", "mcp"])
        .output()
        .expect("relative-root invocation");
    assert_eq!(relative.status.code(), Some(2));
    assert!(relative.stdout.is_empty());

    let eof = Command::new(binary())
        .args([
            "--root",
            fixture("src-layout").to_str().expect("UTF-8 fixture"),
            "mcp",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("pre-initialize EOF invocation");
    assert!(eof.status.success());
    assert!(eof.stdout.is_empty());
    assert!(eof.stderr.is_empty());

    let repository = tempdir().expect("non-Git repository");
    write(repository.path(), "module.py", "VALUE = 1\n");
    write(repository.path(), "excluded.py", "VALUE = 3\n");
    write(
        repository.path(),
        "pyproject.toml",
        "[tool.urmare]\nexclude = [\"excluded.py\"]\n",
    );
    let outside = tempdir().expect("outside directory");
    write(outside.path(), "outside.py", "VALUE = 2\n");
    let (mut server, _) = McpProcess::start(&repository.path().join("."));
    server.notify_initialized();

    for (id, path) in [
        (2, "../outside.py".to_owned()),
        (
            3,
            outside
                .path()
                .join("outside.py")
                .to_string_lossy()
                .into_owned(),
        ),
        (4, "missing.py".to_owned()),
        (5, "excluded.py".to_owned()),
    ] {
        let response = server.call(
            json!(id),
            "validation_plan",
            json!({"source":{"kind":"files","files":[path]}}),
        );
        assert_eq!(response["result"]["isError"], true);
    }
    let git_on_non_git = server.call(
        json!(6),
        "validation_plan",
        json!({"source":{"kind":"changed"}}),
    );
    assert_eq!(git_on_non_git["result"]["isError"], true);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            outside.path().join("outside.py"),
            repository.path().join("escape.py"),
        )
        .expect("symlink escape");
        let symlink = server.call(
            json!(7),
            "validation_plan",
            json!({"source":{"kind":"files","files":["escape.py"]}}),
        );
        assert_eq!(symlink["result"]["isError"], true);
    }

    let (status, stdout, stderr) = server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());

    let git_repository = tempdir().expect("Git-root mismatch repository");
    write(git_repository.path(), "nested/module.py", "VALUE = 1\n");
    git(git_repository.path(), &["init", "--quiet"]);
    git(git_repository.path(), &["add", "."]);
    commit(git_repository.path(), "base");
    let (mut nested_server, _) = McpProcess::start(&git_repository.path().join("nested"));
    nested_server.notify_initialized();
    let mismatched = nested_server.call(
        json!(2),
        "validation_plan",
        json!({"source":{"kind":"changed"}}),
    );
    assert_eq!(mismatched["result"]["isError"], true);
    let (status, stdout, stderr) = nested_server.finish();
    assert!(status.success());
    assert!(stdout.is_empty());
    assert!(stderr.is_empty());
}

fn successful_structured(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "protocol error: {response}"
    );
    assert_eq!(response["result"]["isError"], false);
    let structured = response["result"]["structuredContent"].clone();
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("text content block");
    assert_eq!(
        serde_json::from_str::<Value>(text).expect("JSON text fallback"),
        structured
    );
    structured
}

fn cli_json(root: &Path, arguments: &[&str]) -> Value {
    let output = Command::new(binary())
        .args(["--root", root.to_str().expect("UTF-8 root")])
        .args(arguments)
        .output()
        .expect("run CLI command");
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI JSON")
}

fn initialized_git_repository() -> TempDir {
    let repository = tempdir().expect("temporary Git repository");
    write(
        repository.path(),
        "pyproject.toml",
        "[tool.urmare]\nsource-roots = [\"src\"]\ntest-roots = [\"tests\"]\n",
    );
    for (path, contents) in [
        ("src/pkg/__init__.py", ""),
        ("src/pkg/deleted.py", "DELETED = True\n"),
        ("src/pkg/deleted_user.py", "from . import deleted\n"),
        ("src/pkg/old.py", "OLD = True\n"),
        ("src/pkg/old_user.py", "from . import old\n"),
        ("src/pkg/kept.py", "KEPT = True\n"),
        ("tests/test_deleted.py", "from pkg import deleted_user\n"),
        ("tests/test_old.py", "from pkg import old_user\n"),
    ] {
        write(repository.path(), path, contents);
    }
    git(repository.path(), &["init", "--quiet"]);
    git(repository.path(), &["add", "."]);
    commit(repository.path(), "base");
    repository
}

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("file parent")).expect("create fixture directory");
    fs::write(path, contents).expect("write fixture file");
}

fn git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("Git available");
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
        .expect("Git available");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("UTF-8 Git output")
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

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read source directory") {
        let entry = entry.expect("directory entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("entry type").is_dir() {
            fs::create_dir_all(&target).expect("copy directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}

fn repository_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).expect("snapshot directory") {
            let entry = entry.expect("snapshot entry");
            if entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type().expect("snapshot type").is_dir() {
                visit(root, &entry.path(), snapshot);
            } else {
                snapshot.insert(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("repository-relative path")
                        .to_path_buf(),
                    fs::read(entry.path()).expect("snapshot file"),
                );
            }
        }
    }
    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

#[cfg(unix)]
fn install_fake_pytest(directory: &Path, marker: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let executable = directory.join("pytest");
    fs::write(
        &executable,
        format!("#!/bin/sh\nprintf executed > '{}'\n", marker.display()),
    )
    .expect("fake pytest");
    let mut permissions = fs::metadata(&executable)
        .expect("fake pytest metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(executable, permissions).expect("fake pytest permissions");
}

#[cfg(windows)]
fn install_fake_pytest(directory: &Path, marker: &Path) {
    fs::write(
        directory.join("pytest.bat"),
        format!("@echo executed > \"{}\"\r\n", marker.display()),
    )
    .expect("fake pytest");
}
