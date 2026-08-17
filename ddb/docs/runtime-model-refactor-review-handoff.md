# Runtime-model refactor review handoff

Date: 2026-07-22 UTC

## Review objective

Review the uncommitted refactor that strengthens DDB's build/CI gates and makes `RuntimeModel` the exclusive ownership and coordination boundary for mutable debugger-domain state.

The review should answer:

1. Does the new boundary preserve existing behavior and wire formats?
2. Are activation, group operations, transactions, and retirement correctly serialized?
3. Can mutable manager or session handles still escape the `state` module?
4. Are the CI gates strict, reproducible, and correctly scoped?
5. Is any follow-up required before creating an auditable commit?

## Repository state

- Repository: `/home/ybyan/projs/DDB`
- Branch: `refactor-temp`
- Review base: `1b27d9c6` (`refactor: compose the command services in the application root`)
- The implementation is **not staged or committed**.
- Before this handoff file was added, the tracked diff contained 43 files with 1,069 insertions and 576 deletions. `ddb/docs/runtime-architecture.md` was also untracked.
- Treat every existing worktree change as part of this review. Do not reset or overwrite it.

Useful audit commands:

```bash
git status --short
git diff --check
git diff --stat 1b27d9c6
git diff 1b27d9c6 -- ddb/core/src/state
git diff 1b27d9c6 -- ddb/core/src/session ddb/core/src/cmd_flow
git diff 1b27d9c6 -- .github/workflows/rust-check.yml
```

## Intended architecture

`ApplicationServices::build` creates one `Arc<RuntimeModel>` for an application runtime. Command, API, session, source-resolution, and event-reduction services receive that facade rather than individual repository handles.

`RuntimeModel` now directly owns:

- `StateMgr`
- `GroupMgr`
- `BreakpointMgr`
- `ProcletMgr`
- `GroupOperationCoordinator`

The manager modules are private under `state/mod.rs`. Only domain values, immutable snapshots, narrow read views, and the `RuntimeModel` facade are re-exported to the rest of the crate.

The intended access rules are:

- Mutations use domain-named `RuntimeModel` commands.
- Data crossing an `await` or presentation boundary uses immutable snapshots.
- Latency-sensitive thread-ID projection may use the short-lived read-only `ThreadIdView`.
- No caller may obtain a manager, `SessionRef`, mutable session guard, or group-gate registry.
- Cross-repository mutation ordering belongs in `RuntimeModel`, not in transport or presentation services.

The detailed design is documented in `ddb/docs/runtime-architecture.md`.

## Major implementation areas

### 1. Runtime ownership facade

Primary files:

- `ddb/core/src/state/runtime_model.rs`
- `ddb/core/src/state/mod.rs`
- `ddb/core/src/state/state_mgr.rs`
- `ddb/core/src/state/group_mgr.rs`
- `ddb/core/src/state/group_operation.rs`
- `ddb/core/src/state/bkpt_mgr.rs`
- `ddb/core/src/state/proclet_mgr.rs`

Important changes:

- Removed public manager getters from `RuntimeModel`.
- Removed the extra `Arc` around each manager; the single `Arc<RuntimeModel>` is the shared ownership unit.
- Added `SessionSnapshot` and breakpoint/proclet/group snapshot queries.
- Added controlled session-context, topology, selection, breakpoint, proclet, and group operations.
- Added opaque `GroupOperationGuard` and `GroupOperationSet` values.
- Added `PendingSessionRetirement`, which holds the relevant group gate and consumes itself to perform retirement.
- Made state submodules private and replaced wildcard re-exports with an explicit domain-facing list.

### 2. Activation and retirement ordering

Primary files:

- `ddb/core/src/session/activation.rs`
- `ddb/core/src/state/runtime_model.rs`
- `ddb/core/src/state/group_mgr.rs`
- `ddb/core/src/state/group_operation.rs`

Activation now:

1. Reserves or finds the group ID.
2. Acquires the group operation gate.
3. Revalidates the reservation in case retirement removed and recreated the group while activation waited.
4. Publishes session membership only while holding the correct gate.
5. Synchronizes inherited group breakpoints and completes bootstrap.
6. Adds the router session before releasing the gate.

This ordering prevents an already-running group command from seeing a newly registered but not-yet-routable session.

Retirement now:

1. Acquires the session's group gate through `begin_session_retirement`.
2. Removes router visibility.
3. Consumes the retirement token to clean session, group, breakpoint-target, proclet-owner, thread, and selection state.
4. Removes an emptied group's gate only after model cleanup.

Review cancellation safety carefully. In particular:

- Cancellation after `GroupMgr::ensure_group` but before membership publication may leave an empty reserved group.
- Dropping `PendingSessionRetirement` without calling `finish` releases its gate without retiring the session.
- Cancellation during `finish` should be evaluated for partially completed cross-repository cleanup.

These are explicit review questions, not claims that the current code is wrong.

### 3. Transactions and command services

Primary files:

- `ddb/core/src/cmd_flow/transaction.rs`
- `ddb/core/src/cmd_flow/execution.rs`
- `ddb/core/src/cmd_flow/backtrace.rs`
- `ddb/core/src/cmd_flow/breakpoint.rs`
- `ddb/core/src/cmd_flow/query.rs`
- `ddb/core/src/cmd_flow/event/reducer.rs`
- `ddb/core/src/cmd_flow/router.rs`

Important changes:

- `SessionTransaction` no longer exposes a `SessionRef`.
- Transactions retain ordered exclusive runtime leases and invoke controlled model commands for session state.
- Execution and distributed-backtrace flows use session IDs and snapshots rather than mutable session references.
- Breakpoint mutation and group-operation locking are routed through `RuntimeModel`.
- The event reducer performs topology and breakpoint projection through the facade.
- Query projection still uses the narrow thread-ID read view to avoid copying hot indexes.

Review that no snapshot is assumed to be globally atomic across multiple sessions. `session_snapshots()` intentionally reads sessions sequentially and returns detached values.

### 4. API, diagnostics, source resolution, and proclets

Primary files:

- `ddb/core/src/api/read_model.rs`
- `ddb/core/src/api/server.rs`
- `ddb/core/src/cmd_flow/diagnostics.rs`
- `ddb/core/src/source/resolver.rs`
- `ddb/core/src/feature/proclet_restore.rs`
- `ddb/core/src/app/services.rs`

Important changes:

- API responses are built from model snapshots.
- Diagnostics print snapshots instead of manager internals.
- `RuntimeModel` implements the source resolver's narrow `GroupResolutionView`.
- Proclet restoration resolves ownership through the model.
- The composition root now passes one runtime model throughout the service graph.

Check API/MI serialization and ordering for accidental wire-shape changes. Existing API and integration tests passed, but this remains a high-value review area.

### 5. Build, profile, and CI gates

Primary files:

- `.github/workflows/rust-check.yml`
- `ddb/core/tests/README.md`
- `ddb/docs/runtime-architecture.md`
- `ddb/docs/benchmark-suite.md`

The workflow now runs:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
```

The split test strategy is deliberate:

- Workspace/all-feature **compilation** verifies all feature combinations.
- Workspace tests use default features because `gdbmi` features `test_rr` and `test_rd` unconditionally launch optional `rr`/`rd` executables.
- DDB's own all-feature tests run separately and include the `profile` feature.
- A direct `cargo test --workspace --all-targets --all-features` fails in an environment without `rr`/`rd`; it should not be substituted for the two test commands above unless CI installs and supports those tools.

The strict Clippy command uses `--no-deps` so the DDB gate does not inherit the independent lint backlog of the local `gdbmi` dependency.

### 6. Mechanical strict-Clippy cleanup

The stronger gate exposed pre-existing warnings. Behavior-preserving cleanups were made in files including:

- `ddb/core/src/cmd_flow/input.rs`
- `ddb/core/src/cmd_flow/session_runtime/actor.rs`
- `ddb/core/src/common/config.rs`
- `ddb/core/src/common/utils.rs`
- `ddb/core/src/debugger/gdb/command.rs`
- `ddb/core/src/debugger/gdb/parser.rs`
- `ddb/core/src/feature/proclet_ctrl.rs`
- `ddb/core/src/notification/manager.rs`
- `ddb/core/src/status.rs`
- `ddb/core/tests/support/mod.rs`

Review these separately from the ownership refactor. Notable examples are derived `Default` implementations, a `Display` implementation replacing an inherent `to_string`, parser simplifications, and localized Clippy exceptions for serialized legacy acronyms, module inception, and the composition constructor's argument count.

## New or strengthened invariant tests

`state/runtime_model.rs` includes tests covering:

- retirement of the last group member
- retirement while another group member remains
- custom-context changes through model commands
- retirement waiting for an in-flight group operation
- activation withholding group membership until it owns the group gate

Related existing tests cover group-gate serialization, session cleanup, late-joining breakpoint inheritance, transaction lease ordering, topology cleanup, and independent application-runtime state.

## Validation evidence

The following commands passed on 2026-07-22:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
cargo test -p ddb --all-features state::runtime_model
cargo build -p ddb --release --all-features
git diff --check
```

The final DDB all-feature unit run reported 177 passed and 1 ignored. All mock integration and real GDB integration tests also passed. The targeted runtime-model run reported all 5 tests passing.

The boundary audit returned no manager or `SessionRef` references outside `state`:

```bash
rg -n 'Arc<(StateMgr|GroupMgr|BreakpointMgr|ProcletMgr)>|\bSessionRef\b|\b(BreakpointMgr|StateMgr|GroupMgr|ProcletMgr)\b' \
  ddb/core/src --glob '*.rs' --glob '!**/state/*.rs'
```

## Performance evidence

Benchmark command, used identically before and after:

```bash
cargo run -p ddb-bench --release -- \
  --scenarios api-thread-info,api-thread-info-burst,api-list-groups \
  --scales 1,16 \
  --threads-per-session 4 \
  --samples 12 \
  --warmup 2 \
  --format json
```

Baseline to final p50/p95 latency in milliseconds:

| Scenario | Sessions | Baseline | Final | Delta |
|---|---:|---:|---:|---:|
| `api-thread-info` | 1 | 0.290 / 0.452 | 0.280 / 0.456 | -3.4% / +1.1% |
| `api-thread-info` | 16 | 1.147 / 1.214 | 1.218 / 1.241 | +6.1% / +2.2% |
| `api-thread-info-burst` | 1 | 0.747 / 0.913 | 0.743 / 0.961 | -0.6% / +5.3% |
| `api-thread-info-burst` | 16 | 8.328 / 8.606 | 8.174 / 8.713 | -1.9% / +1.2% |
| `api-list-groups` | 1 | 0.172 / 0.326 | 0.156 / 0.317 | -9.2% / -3.0% |
| `api-list-groups` | 16 | 0.452 / 0.505 | 0.433 / 0.544 | -4.1% / +7.9% |

No material regression was observed. One intervening run produced a 16-session `api-thread-info` p95 of 1.421 ms (+17%, +0.208 ms), while the heavier 16x16 burst remained stable. A repeated identical run returned that p95 to 1.241 ms (+2.2%), so the isolated point was treated as run noise rather than a repeatable scaling regression.

## Suggested review order

1. `state/mod.rs` and `state/runtime_model.rs`: verify the boundary and public surface.
2. `state/group_mgr.rs`, `state/group_operation.rs`, and `session/activation.rs`: reason about activation/retirement races and cancellation.
3. `cmd_flow/transaction.rs`, `execution.rs`, and `backtrace.rs`: verify lease/state ordering.
4. `cmd_flow/breakpoint.rs`, `event/reducer.rs`, and `router.rs`: verify group gates and cross-store mutation ordering.
5. API, diagnostics, source-resolution, and proclet consumers: verify snapshot semantics and wire compatibility.
6. CI and mechanical lint cleanups: confirm the gate policy and behavior-preserving edits.
7. Re-run the validation commands before authorizing a commit.

## Commit guidance

No auditable commit exists yet. After review findings are resolved and validation is repeated, either:

- create one cohesive commit such as `refactor: enforce runtime model ownership boundary`, or
- split the work into an initial CI/lint-gate commit followed by the runtime ownership refactor, if the split can be made without leaving either commit failing its own gates.

Do not describe the change as committed until `git status`, the staged diff, and the resulting commit are explicitly verified.
