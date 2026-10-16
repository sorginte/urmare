# Local MCP integration

Urmare can run as a local, read-only Model Context Protocol server for one
Python repository. Coding agents can request the same deterministic validation
plan and dependency explanation available through the CLI, without generating
or parsing Urmare commands.

Urmare plans validation; it does not execute pytest. The agent must run every
returned step through the repository's established environment.

## Install and start

Use a persistent tool installation for a long-running MCP server:

```bash
uv tool install urmare
# or
pipx install urmare
```

MCP support requires Urmare 0.4.0 or newer. Upgrade an existing installation
with `uv tool upgrade urmare` or `pipx upgrade urmare`, then check
`urmare --version` before configuring a client.

Start the server with an absolute repository path:

```bash
urmare --root /absolute/path/to/repository mcp
```

`--root` is mandatory for MCP. Urmare resolves and canonicalizes it before
initialization and binds the process to that root for its entire lifetime. Tool
requests cannot supply or replace the root, and the server never falls back to
the MCP client's working directory.

For a GUI client that does not inherit the shell's `PATH`, find the executable
and use its absolute path:

```bash
command -v urmare
# Windows PowerShell: (Get-Command urmare).Source
```

Avoid using ephemeral `uvx` resolution as the primary server command. A
persistent `uv tool` or `pipx` installation has a stable executable path and
does not need package resolution during client startup.

## Client configuration

Replace both absolute paths in these examples. The formats below follow the
respective vendors' documented stdio MCP configuration. The protocol exchange
and packaged executable are tested by Urmare; live Codex, Claude Code, and
Cursor applications were not available for end-to-end verification in the
development environment for this feature.

### Codex

Register the server from the command line:

```bash
codex mcp add urmare -- /absolute/path/to/urmare \
  --root /absolute/path/to/repository mcp
codex mcp list
```

Or add it to `~/.codex/config.toml` (or a trusted project's
`.codex/config.toml`):

```toml
[mcp_servers.urmare]
command = "/absolute/path/to/urmare"
args = ["--root", "/absolute/path/to/repository", "mcp"]
```

See the [official Codex MCP documentation](https://developers.openai.com/docs/extend/mcp).

### Claude Code

Register a local stdio server:

```bash
claude mcp add urmare -- /absolute/path/to/urmare \
  --root /absolute/path/to/repository mcp
```

For a project-scoped `.mcp.json`:

```json
{
  "mcpServers": {
    "urmare": {
      "command": "/absolute/path/to/urmare",
      "args": ["--root", "/absolute/path/to/repository", "mcp"]
    }
  }
}
```

See the [official Claude Code MCP documentation](https://docs.anthropic.com/en/docs/claude-code/mcp).

### Cursor

Add `.cursor/mcp.json` to the project, or use `~/.cursor/mcp.json` for a global
entry:

```json
{
  "mcpServers": {
    "urmare": {
      "type": "stdio",
      "command": "/absolute/path/to/urmare",
      "args": ["--root", "/absolute/path/to/repository", "mcp"]
    }
  }
}
```

See the [official Cursor MCP documentation](https://cursor.com/docs/context/mcp).

## Agent workflow

The intended loop is:

```text
agent edits Python files
    ↓
agent calls validation_plan
    ↓
agent executes every returned validation step
    ↓
agent calls explain_impact when a relationship needs investigation
    ↓
CI independently recomputes the plan
```

The returned pytest action is structural:

```json
{
  "kind": "pytest",
  "program": "pytest",
  "args": ["tests/api/test_checkout.py"]
}
```

An agent maps that action into the repository's normal environment, for
example `uv run pytest tests/api/test_checkout.py`. Urmare never invokes the
program itself.

## Tools

The server advertises exactly two tools. Both are annotated read-only,
non-destructive, and closed-world.

### `validation_plan`

Returns deterministic Python impact and pytest validation for exactly one
change source:

```json
{"source":{"kind":"files","files":["src/payments/service.py"]}}
```

```json
{"source":{"kind":"changed"}}
```

```json
{"source":{"kind":"git_diff","base":"origin/main"}}
```

The successful `structuredContent` is exactly the schema-version-1 DTO emitted
by the equivalent `urmare plan ... --json` command. Modes are `selective`,
`none`, or `full`. A compatibility text content block contains a JSON
serialization of that same value.

### `explain_impact`

Returns one deterministic dependency path and its static import evidence:

```json
{
  "changed_file": "src/payments/stripe.py",
  "affected_file": "tests/api/test_checkout.py",
  "context": {"kind": "current"}
}
```

The required context can also be `{"kind":"changed"}` or
`{"kind":"git_diff","base":"origin/main"}`. Making context explicit avoids
using a current-tree graph accidentally when deleted paths or previous rename
identities require Git-aware analysis. Successful `structuredContent` is
identical to `urmare why ... --json`.

Input and output JSON Schemas are generated from the Rust contract types and
advertised by `tools/list`. Inputs reject unknown and conflicting fields.
Arrays and result paths are deterministic, complete, normalized, and
repository-relative.

## Read-only and security boundaries

The canonical root supplied at startup is the server's filesystem capability
boundary. Urmare reuses the CLI's protections for relative escapes, absolute
outside paths, symlinks resolving outside the root, invalid virtual Git paths,
excluded or missing files, and invalid Git bases. Git is invoked with argument
arrays, never through a shell.

The server:

- never accepts arbitrary commands or request-level roots;
- never executes pytest or installs dependencies;
- never makes network requests;
- never exposes arbitrary filesystem resources, prompts, or sampling;
- never creates or modifies files inside the analyzed repository.

Urmare may update its private, per-user operating-system cache. Every tool call
refreshes analysis through that persistent index. Clean calls reuse it, edits
receive bounded incremental processing where supported, and unsafe or damaged
cache state falls back to correct analysis.

Analysis calls are serialized inside one server process. A tool error does not
terminate the server, so a corrected request can follow it.

## Transport and errors

Only local stdio transport is supported. Stdout contains JSON-RPC protocol
traffic only; operational diagnostics go to stderr. Normal stdin EOF closes
the server successfully.

Malformed protocol messages and invalid argument structures return JSON-RPC
errors. Valid tool requests that cannot be analyzed return a tool result with
`isError: true` and an actionable text message. Examples include an unindexed
path, a non-Git root used with a Git mode, an invalid base, a missing dependency
path, or a configuration-driven explanation refusal.

## Troubleshooting

- **Client cannot start the server:** use the absolute Urmare executable path
  and an absolute existing repository root.
- **Git-aware request fails:** confirm the bound root is the intended Git top
  level and the base exists locally (`git rev-parse --verify BASE`).
- **File is not indexed:** pass a repository-relative Python path that is
  inside configured roots and not excluded by `tool.urmare.exclude`.
- **Plan mode is `full`:** inspect `full_validation`; a changed root
  `pyproject.toml` intentionally requires the complete test suite.
- **Client shows only text:** the text block is the same JSON value as
  `structuredContent`, supplied for protocol-client compatibility.
- **Unexpected result after another edit:** recompute the plan. Each call
  refreshes the index, but a previously returned plan is an immutable snapshot.

Remote/hosted transports, authentication, test execution, shell access,
resources, prompts, and agent-specific completion enforcement are deliberately
out of scope for this local integration.
