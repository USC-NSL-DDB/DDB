# Integration Tests

These tests live under `core/tests` and are intended to exercise DDB as a real process, not just individual Rust units.

Run all integration and unit tests from the repository root with:

```bash
cargo test -p ddb
```

## Cargo Aliases

The workspace defines a few convenience aliases in `.cargo/config.toml`:

```bash
cargo xtest-unit
cargo xtest-integration
cargo xtest-integration-mock
cargo xtest-integration-real
```

These map to:

- `cargo xtest-unit`: workspace unit tests only
- `cargo xtest-integration`: all `ddb` integration tests only
- `cargo xtest-integration-mock`: mock integration tier only
- `cargo xtest-integration-real`: all real GDB/LLDB integration tests

## Test Tiers

There are two integration-test tiers.

### 1. Mock integration tests

These tests use the mock debugger backend and are fast and deterministic:

- `session_bootstrap.rs`
- `breakpoint_sync.rs`
- `breakpoint_validation.rs`
- `command_routing.rs`
- `session_cleanup.rs`

Run only this tier with:

```bash
cargo test -p ddb --test session_bootstrap --test breakpoint_sync --test breakpoint_validation --test command_routing --test session_cleanup
```

### 2. Real debugger integration tests

These tests launch or attach to real local binaries through GDB and LLDB:

- `real_session_bootstrap.rs`
- `real_breakpoint_sync.rs`
- `real_session_cleanup.rs`
- `real_lldb_session_bootstrap.rs`
- `real_distributed_backtrace.rs`
- `real_faketime.rs`

Run only this tier with:

```bash
cargo xtest-integration-real
```

If you want to see the live stdout from DDB during a run, add `-- --nocapture`:

```bash
cargo test -p ddb --test real_session_bootstrap -- --nocapture
```

## Requirements

The mock tier has no external runtime dependency beyond Rust.

The real tier requires:

- `gdb` to be installed and available on `PATH`
- `lldb` to be installed and available on `PATH`
- `libfaketimeMT.so.1` from the `libfaketime` package
- `cargo` to be available on `PATH`
- a Linux environment

The real suite is designed primarily for `x86_64` and `aarch64`.

The FAKETIME tests search standard Debian/Ubuntu and local-user library paths.
Set `LIBFAKETIME_PATH=/absolute/path/to/libfaketimeMT.so.1` when the library is
installed elsewhere.

## How The Real Tier Works

You do not need to manually build or start a dummy application.

The shared harness in `support/mod.rs` will:

1. Build the fixture crate in `fixtures/real_loop`
2. Start the real `ddb` binary as a subprocess
3. Generate a temporary config with static sessions in `start_mode: binary`
4. Let DDB launch or attach to the fixture through local GDB or LLDB
5. Drive DDB through stdin and validate behavior through stdout plus the HTTP API

The fixture is intentionally simple and architecture-neutral:

- breakpoints are inserted by source file and line number
- tests assert MI/API behavior and compare captured register addresses when
  verifying context restoration; they do not assume fixed instruction addresses

The attach fixture explicitly allows the sibling debugger relationship under
Linux Yama `ptrace_scope=1`. Production targets remain subject to their normal
host ptrace policy.

## Running One Scenario

Examples:

```bash
cargo test -p ddb --test session_bootstrap
cargo test -p ddb --test breakpoint_sync
cargo test -p ddb --test real_breakpoint_sync -- --nocapture
```

## CI

GitHub Actions runs the full workspace suite through
`.github/workflows/rust-check.yml` on `ubuntu-latest`. The workflow installs
the stable Rust toolchain and runs formatting, all-target/all-feature
compilation, strict Clippy checks for the DDB package, and:

```bash
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
```

## LLDB gRPC validation

The instrumented GCC-built greeter binaries require LLDB 20 or later. LLDB 18
rejects `DW_FORM_data16` and leaves their source breakpoints unresolved. On Ubuntu
24.04, install `lldb-20 python3-lldb-20` and make that version available as `lldb`
on each target host. Discovered targets use SSH, so check its executable lookup
there as well. The integration harness clears `DEBUGINFOD_URLS` to keep remote
symbol servers out of local test timing.

The LLDB regressions cover:

- Stopping at application `main` for `stop_at_entry`, with an entry stop reason.
- Sending signals to a paused process, including terminating it with SIGKILL.
- Updating the cached frame address after restoring saved registers. An SBValue
  register write alone updates the register but leaves LLDB's frame cache stale;
  the bridge finishes with `SBFrame.SetPC` to refresh the frame and unwinder.
- Distributed stacks after callers have executed beyond their saved contexts.
- Launch, attach, variables, memory, registers, and typed API operations.

Signal delivery honors LLDB's signal stop/pass policy. A stopped local target
receives a queued host signal before resuming, because LLDB's asynchronous
`SBProcess.Signal` path rejects stopped targets. The bridge runs on the target
host for SSH sessions too. Runtime startup signal 40 wakes `sigwait`, after
which the DDB connector raises SIGTRAP to establish the initial pause. Signal 0
is not a supported delivery operation; Continue is a separate operation.

Run the focused backend checks from this worktree:

```sh
CARGO_TARGET_DIR=/tmp/ddb-canonical-build cargo test --manifest-path ddb/Cargo.toml \
  -p ddb --bin ddb --test api_v2_real_backends \
  --test real_lldb_session_bootstrap --test real_distributed_backtrace
python3 ddb/core/tests/lldb_breakpoint_options.py
```

The adapter repository contains `examples/grpc/` profiles and an extension-host
scenario that drives the actual greeter client and server. That scenario is
required in addition to the synthetic fixtures to validate C++ RPC caller
frames, VS Code selection, and attached-process cleanup.
