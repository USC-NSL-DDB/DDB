# Single-file DDB and ddb-tui configuration

## Delivered requirements

The normal managed workflow takes one explicit project YAML:

```bash
ddb tui .ddb/ddb.yaml
# Equivalent when the frontend can resolve the paired backend:
ddb-tui .ddb/ddb.yaml
```

- `Tui.source_roots` supplies frontend-private source trees before attachment.
- DDB `SourceRoots` remains an explicit API export on the backend host.
- `Conf.Debugger` owns backend-specific commands, debugger-host source mappings,
  pretty printing, and an optional native temporary entry breakpoint.
- Persistent setup executes once per session bootstrap, including auto-attach.
  Default frontend connections and refreshes do not configure the debugger.
- `--ddb-set KEY=VALUE` forwards typed overrides to DDB's canonical config loader.
- `--tui-config` is an optional preferences file, not a required second config.
- The optional API mutation profile is named `--session-profile`; the former
  `--debugger-settings` flag is removed without a compatibility alias.

The shared `ddb-config` crate defines source roots, native setup, and frontend
preferences. It does not execute commands or open source directories. The
backend still owns its complete configuration schema and validation.

## Ownership, precedence, and failure rules

Frontend precedence is defaults, inline project `Tui`, explicit frontend
preferences, then explicit CLI options. Root lists replace rather than merge.
`--source-workspace` replaces the resolved local root list. Relative frontend
paths and backend source-export paths resolve against their containing file.
Debugger source mappings require absolute debugger-host paths.

Backend overrides apply in command-line order before typed deserialization.
Unknown keys and invalid types fail rather than silently disappearing. Managed
mode rejects replacing `Conf`, its `api_*` settings, or frontend `Tui` through
backend overrides. The supervisor retains private authentication, loopback
transport, atomic readiness, and lifecycle ownership. Values remain argument
strings; neither debuggee arguments nor configuration overrides use a shell.

Only explicitly supplied configuration files are loaded. Native commands are
trusted code with debugger-console authority. Commands run after attach or
symbol loading, before execution and framework post-start actions. A bootstrap
failure prevents activation and stops later commands. Already applied side
effects are not rolled back.

The loader reads regular files with a 1 MiB bound. Native commands and mappings
have count and text bounds. Existing session termination and token protections
remain in place. Frontend-local grants never reach DDB's source store.

## Related safety correction

Managed Mosquitto previously daemonized and stopped processes by name. DDB now
retains its own child process, refuses an occupied port, waits for bounded
readiness, and kills/reaps only that child. Repeated shutdown is safe. The
default listener uses the configured port; the unused fixed-port embedded
configuration was removed. A regression confirms an unrelated process survives.

Strict parsing also exposed the unsupported `Pids` key in `pidconfig.yaml`.
The example now uses `StaticSessions`; no ignored-key compatibility path was
added.

The final example audit found nine historical `Components` configs and a typo
in the custom Mosquitto config example. The historical files are preserved in
`ddb/core/configs/archive` as `.yaml.txt`, with their unsupported behavior
documented. The current Mosquitto example uses `config_path` and explains its
required listener settings. A regression parses every active project YAML in
`ddb/core/configs` and the unified helloworld example through the canonical
backend loader. The installed-broker smoke test no longer references the
removed asset or checks unrelated processes by name.

## Reproducing verification

Final results on this checkout:

| Check | Result |
| --- | --- |
| Shared configuration unit tests | 4 passed |
| Backend unit suite | 350 passed, 2 pre-existing ignored tests |
| Installed Mosquitto smoke test | 1 additional ignored-by-default test passed |
| Frontend unit suite | 338 passed |
| Full native terminal suite | 36 passed, including 5 real greeter workflows |
| Exact application-local helloworld config | 1 additional real workflow passed |
| SDK, API security, source exports, managed startup | 19 integration tests passed |
| Frontend and shared-config Clippy | Passed with warnings denied |
| Rust formatting and patch whitespace | Passed |

An earlier complete run had one expected test failure because it waited for the
removed automatic frontend `ConfigureDebugger` operation. The updated regression
asserts no such operation occurs on either backend. The final complete suite
passed. No timeout was increased to hide that change.

```bash
cargo test --manifest-path ddb/Cargo.toml --offline -p ddb-config
cargo test --manifest-path ddb/Cargo.toml --offline -p ddb --bin ddb
cargo test --manifest-path ddb/Cargo.toml --offline -p ddb --bin ddb \
  discovery::tests::test_broker_starter -- --ignored --exact
cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline --bin ddb-tui
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --offline --all-targets -- -D warnings
cargo clippy --manifest-path ddb/Cargo.toml --offline -p ddb-config --all-targets -- -D warnings

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock -- --ignored --test-threads=1
```

The terminal suite uses real GDB and LLDB alongside deterministic mock cases.
The greeter cases use actual instrumented C++ binaries and `ddb_runapp`, not
static or simulated greeter sessions. Run no other discovery session during
these tests. The fixture refuses a live configured broker and preserves/restores
the discovery file, including when it was initially absent.

The single-config greeter test checks local browsing before attach, both
automatic attachments, a real RPC breakpoint, an exact `"Hello "` watch, step
over to the return line, refresh, and owned-broker shutdown. Its generated case
uses `ddb tui` without setup flags. Set `DDB_GREETER_PROJECT_CONFIG` to exercise
an existing one-file project through direct `ddb-tui` instead.

During this implementation, a new ready-to-run project was created at
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld/.ddb/ddb.yaml`. The existing
`dbg_grpc.yaml` and `dbg_grpc_lldb.yaml` were not changed. That application-local
file is outside this repository and is not part of its commits. Its portable
template is `ddb-tui/examples/helloworld.yaml`.

The exact application-local file was verified with:

```bash
DDB_GREETER_WORKSPACE=/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld \
DDB_GREETER_PROJECT_CONFIG=/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld/.ddb/ddb.yaml \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock greeter_auto_attach_single_config_managed_terminal_workflow \
  -- --ignored --test-threads=1 --nocapture
```

No wider performance claim is made. Native formatter initialization and
distributed all-stop can still take several seconds on this checkout.

## Commit trail

1. `30b4b0ed` introduces shared types, strict backend parsing and overrides,
   native GDB/LLDB bootstrap setup, and the corrected PID example.
2. `3d30a722` fixes owned Mosquitto lifecycle and removes its obsolete asset.
3. `5ee4fdc5` adds inline frontend preferences, precedence, safe forwarding,
   private roots, explicit API profiles, and unit regressions.
4. `c6c71c30` adds the managed single-config greeter case,
   discovery-file restoration, and assertions that default connections do not
   mutate debugger configuration.
5. `bd7d5fa1` contains this audit, the updated guides, and the
   portable project and frontend-preferences examples.
6. The final configuration-example cleanup commit preserves unsupported legacy
   examples in an archive, validates every active project example, corrects the
   custom broker example, and updates the installed-broker smoke test.

All commits use explicit file allowlists. No commits are pushed automatically.
