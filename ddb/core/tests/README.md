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
- tests assert MI/API behavior, not register values or instruction addresses

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
