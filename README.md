# DDB

DDB is a distributed debugger backend with GDB, LLDB, and deterministic Mock
support. `ddb-tui` is its modern mouse- and keyboard-friendly terminal frontend.
They are separate binaries that communicate only through the public DDB API, but
the normal experience is one command.

## Quick start

A paired installation puts `ddb` and `ddb-tui` beside each other. Run the
packaged Mock example without starting a backend manually:

```bash
ddb tui examples/managed/mock.yaml
```

Use the same command for a local or distributed DDB configuration:

```bash
ddb tui ./debug.yaml
```

`ddb-tui ./debug.yaml` is equivalent. In both forms the frontend starts a
private DDB service on an OS-assigned loopback port, creates short-lived bearer
credentials, waits for typed readiness, negotiates API v2, and owns cleanup.

For a conventional local program, no YAML is required:

```bash
# Build with debug information; Rust's default dev profile is suitable.
cargo build
ddb tui launch --backend gdb -- ./target/debug/my-app --app-argument
```

Attach to an already running process:

```bash
ddb tui attach --backend gdb --pid 12345
```

Launch defaults to killing the launched program when the TUI exits. Attach
defaults to detaching and leaving the original process alive.

## Backend ownership modes

Use managed mode for ordinary local and distributed debugging:

```text
ddb tui CONFIG
ddb tui launch -- PROGRAM [ARGS...]
ddb tui attach --pid PID
```

Use `connect` only when another lifecycle deliberately owns DDB—for example a
service manager, container, shared team service, or headless automation:

```bash
ddb tui connect --api https://debug.example.com
# Direct frontend form:
ddb-tui connect --api https://debug.example.com --token "$DDB_API_TOKEN"
```

Quitting a connected TUI never shuts down that external backend. The legacy
`ddb-tui --api URL` spelling remains supported during the compatibility window;
API v1 fallback remains explicitly opt-in with `--api-version v1-fallback`.

Run DDB without a frontend when building automation or another community client:

```bash
# Existing interactive stdin plus API behavior:
ddb ./debug.yaml

# Headless API service; stdin EOF does not stop it:
ddb serve ./debug.yaml
```

## Configuration and lifecycle

DDB remains the sole parser and validator of debugger configuration. The TUI
canonicalizes the configuration path and forwards it without importing DDB core
types or interpreting distributed topology.

The existing global policy remains the default:

```yaml
Conf:
  on_exit: detach
```

Mixed configurations may override it per static session:

```yaml
StaticSessions:
  - tag: launched-worker
    start_mode: binary
    binary_path: ./target/debug/worker
    on_exit: kill
  - tag: attached-service
    start_mode: attach
    pid: 12345
    on_exit: detach
```

Managed mode always overrides API exposure to authenticated loopback port `0`.
It never honors a configuration's remote API bind. Start `ddb serve` explicitly
when deploying an externally reachable backend and follow the API transport,
TLS, authentication, and CORS guidance.

## Building from source

The backend and frontend have separate Cargo workspaces:

```bash
cargo build -p ddb --manifest-path ddb/Cargo.toml
cargo build --manifest-path ddb-tui/Cargo.toml
```

During source-tree development they are not in the same target directory, so
select the backend explicitly:

```bash
ddb-tui/target/debug/ddb-tui \
  --ddb-path ddb/target/debug/ddb \
  ddb/examples/managed/mock.yaml
```

Or exercise the dispatcher:

```bash
DDB_TUI_PATH=ddb-tui/target/debug/ddb-tui \
  ddb/target/debug/ddb tui ddb/examples/managed/mock.yaml
```

GDB or LLDB must be installed for real sessions. DDB checks the selected
debugger before a headless service announces readiness. Linux attach may also be
limited by ptrace/Yama policy; DDB reports that condition and never changes host
security policy automatically.

## Public API for community frontends

All first-party TUI reads, mutations, events, output, and DDB-specific features
cross the same public API available to community clients:

- [API v2 contract and transport guide](ddb/docs/api/v2.md)
- public Rust crates: `ddb-api-types`, `ddb-api-client`, and
  `ddb-api-conformance`
- [TypeScript SDK](ddb/sdk/typescript/README.md)
- [Python SDK](ddb/sdk/python/README.md)
- [extension authoring](ddb/docs/api/extension-authoring.md)
- [two-binary/one-command ADR](ddb/docs/api/adr/0006-two-binaries-one-command.md)
- [integrated debugger user guide](ddb/docs/ddb-tui-user-guide.md)
- [integrated 0.1 release notes](ddb/docs/releases/ddb-tui-integrated-0.1.md)
- compatible [API v1](ddb/docs/api-v1.md) and MI-shaped stdin surfaces

Protobuf is the canonical v2 semantic contract. HTTP/ProtoJSON is mandatory;
the binary gRPC transport remains an optional preview. Transport adapters share
one application service and do not reimplement debugger semantics.

## Verification and release candidates

Run backend and frontend gates independently:

```bash
cargo test --workspace --all-targets --manifest-path ddb/Cargo.toml
cargo clippy --manifest-path ddb/Cargo.toml -p ddb \
  --all-targets --all-features --no-deps -- -D warnings
ddb/tools/check-api-release.sh

cargo test --all-targets --manifest-path ddb-tui/Cargo.toml
cargo clippy --all-targets --all-features \
  --manifest-path ddb-tui/Cargo.toml -- -D warnings
```

Build a paired archive and test it outside the workspace:

```bash
ddb/tools/package-ddb-tui-release.sh
ddb/tools/test-ddb-tui-release.sh dist/ddb-*.tar.gz
```

The archive contains both binaries, compatibility metadata, Bash completions,
documentation, and the Mock example. The packaging manifest deliberately marks
the archive ineligible for an official open-source release while this repository
has no project `LICENSE` file. Maintainers must select and add the project
license before publishing an official community artifact.
