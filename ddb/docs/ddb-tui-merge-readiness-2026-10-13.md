# DDB TUI cleanup and merge readiness

The cleanup on `codex/tui-next` uses `b1fe38a4` as its implementation baseline.
The broader review covers the 112 preceding branch commits against
`dev` at `cd9b5b33f742d3f8cd62912a8a83c591d75e4165`. GitHub's `dev` ref was checked
directly and matched that baseline. The user confirmed `dev` as the merge target.
No merge, rebase, push, or pull request was performed.

## Standards

The standards review found three actionable design smells, not violations of a
documented repository coding standard. All three are resolved.

1. Configuration readers duplicated descriptor checks and byte limits. The
   session-profile reader omitted nonblocking open and regular-file validation.
   A typed, size-limited reader in `ddb-config` now handles backend/project YAML,
   frontend workspace YAML, and explicit profiles. Project/workspace limits
   remain 1 MiB; profiles remain 64 KiB. Strict format-specific validation and
   configuration ownership remain separate. The test-only in-memory profile
   parser is no longer compiled into the production binary.
2. Queue-full and worker-closed branches repeated read-state cleanup and intent
   failure handling. They now share one rejection path, while retaining their
   different connection policies. Tracked requests are unwrapped once before
   cleanup. Existing tests exercise both rejection cases.
3. Execution routing converted `Control` to a string, parsed that string back
   into a generated enum, and separately repeated the enum mapping in the
   worker. Capability checks and target selection now accept the generated v2
   `ExecutionAction`. One `Control` mapping supplies the worker and UI. Display
   labels are separate and never participate in routing.

The most serious standards finding was the blocking profile reader. Its CLI
regression failed before the fix and now rejects a FIFO without a writer. The
test bounds its child runtime's worker count, so runtime initialization does not
dominate the file-read check. No production timeout was increased.

## Spec

The requirements review found two correctness gaps. Both are resolved within
the existing frontend and configuration contracts.

The unified-config audit promises regular-file bounded loading. Explicit
`--session-profile` previously bypassed those protections and could block before
startup timeout handling. It now uses the same protected reader and preserves
its stricter size bound.

The target-safety requirements say that replacement instances must not revive
old targets. Previously, retiring operation feedback did not retire the actual
queued mutation. A Broadcast Continue waiting behind a slow operation could
reach a replacement backend at the same URL. The worker regression reproduced
two Execute admissions where only the original request should have been sent.

Feedback now owns one shared retirement guard per observed backend generation.
Entries and queued requests share that guard. Replacement retires the generation
and installs a fresh guard. A same-instance disconnect does not retire it. The
worker checks retirement after message backpressure and before dispatch.

A follow-up regression exposed an incomplete first fix: unconfirmed entries can
leave the bounded display history while their requests remain queued. Guard
lifetime therefore belongs to the backend generation, not the retained history
entries. History eviction no longer releases a queued request from retirement.
This uses the existing feedback lifecycle rather than a second epoch or cache.

The most serious spec finding was stale Broadcast execution after an observed
replacement. The regression now admits only the original request, and the
history-eviction regression also passes.

## Stop owner correction found by real application verification

The first complete real-app run passed 36 terminal workflows but failed the
managed greeter workflow. Its server breakpoint had a live hit while the source
view inspected an automatically interrupted client in `epoll_wait`. The next
greeter case then failed because the shared test lock was poisoned.

The isolated managed workflow reproduced the same failure. A smaller
event-before-snapshot test then reproduced it deterministically: pause-owner
selection ran while the event's initiating thread was still locally marked
running. Selection failed, and a later stopped peer could take focus.

The stop handler now reconciles the initiating stop through the existing
all-stop policy before choosing the pause owner. No new focus policy or timeout
was added. The regression also enumerates the automatic peer first in the next
snapshot and checks that the initiating thread remains selected and that only
one peer interrupt is queued. The deterministic test and the exact managed
greeter workflow pass after the ordering correction.

## Contract limits

The queue guard protects replacements observed by this frontend before dispatch.
It does not provide an atomic fence against an unobserved server restart or a
restart during an in-flight SDK request. The current v2 preconditions contain
resource and state revisions, not an expected server-instance identity. A
backend-enforced instance precondition requires separate API design and
contract changes. This cleanup adds no such protocol guarantee or compatibility
fallback.

Existing local-versus-backend source implementations, opt-in API session
profiles, and frontend preference overrides remain intentional interfaces.
They have distinct ownership and use cases. Historical implementation audits
are retained as review evidence rather than deleted as duplicate code.

## Verification results

| Check | Result |
| --- | --- |
| Frontend unit suite | 378 passed. |
| Non-ignored terminal tests | Five passed, including the FIFO startup regression. |
| Full opt-in terminal suite | 38 passed, including all five real greeter workflows and native GDB/LLDB controls, values, memory, and lifecycle checks. |
| Existing helloworld project config | One additional managed real-app run passed with `.ddb/ddb.yaml`; its SHA-256 remained unchanged. |
| Backend unit suite | 352 passed; two existing tests remain ignored by default. |
| Shared crates and SDK | Five config, 19 source-workspace, and 16 Rust SDK tests passed. |
| API integration checks | 18 passed across source/security, targeting, and specification conformance. |
| Strict Clippy | Frontend, backend, shared config/source, and Rust SDK all passed with warnings denied. |
| Generated API artifacts | `ddb-api-codegen --check` reproduced them exactly. No generated files changed. |
| Formatting and patch whitespace | Passed. |
| Test process cleanup | Discovery port 10101 was free; no broker, DDB, TUI, greeter, or real fixture process remained. |

The fixed-discovery real tests used the instrumented applications in
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld`. The first full terminal
run's 36-pass result is superseded by the 38-pass rerun after the stop-order fix.

## Reproducing checks

```bash
cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline
cargo clippy --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --all-targets --all-features --no-deps -- -D warnings
cargo fmt --manifest-path ddb-tui/Cargo.toml -- --check

cargo test --manifest-path ddb/Cargo.toml --locked --offline -p ddb --bin ddb
cargo test --manifest-path ddb/Cargo.toml --locked --offline \
  -p ddb-config -p ddb-source-workspace -p ddb-api-client --lib
cargo test --manifest-path ddb/Cargo.toml --locked --offline -p ddb \
  --test api_v2_sources --test api_v2_source_security \
  --test api_v2_targeting --test api_v2_spec_conformance
cargo clippy --manifest-path ddb/Cargo.toml --locked --offline \
  -p ddb -p ddb-config -p ddb-source-workspace -p ddb-api-client \
  --all-targets --all-features --no-deps -- -D warnings
cargo run --manifest-path ddb/Cargo.toml --locked --offline \
  -p ddb-api-codegen -- --check
cargo fmt --manifest-path ddb/Cargo.toml --all -- --check
git diff --check

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_E2E_BIN=/absolute/path/to/DDB/ddb/target/debug/ddb \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock -- --ignored --test-threads=1

DDB_GREETER_WORKSPACE=/absolute/path/to/grpc/examples/cpp/helloworld \
DDB_GREETER_PROJECT_CONFIG=/absolute/path/to/helloworld/.ddb/ddb.yaml \
DDB_E2E_BIN=/absolute/path/to/DDB/ddb/target/debug/ddb \
  cargo test --manifest-path ddb-tui/Cargo.toml --locked --offline \
  --test e2e_mock greeter_auto_attach_single_config_managed_terminal_workflow \
  -- --ignored --exact --test-threads=1
```

Build the sibling backend before real-app checks. Use the fixture build commands
in the [TUI README](../../ddb-tui/README.md#end-to-end-tests). Greeter discovery
checks must run serially with no existing broker on their configured port.

## Commit trail

1. `f3e3f8a6`, `refactor(config): share bounded typed YAML loading`.
   Shared reader, callers, profile validation, and file/startup regressions.
2. `89902095`, `refactor(tui): centralize execution dispatch and intent lifetime`.
   Typed execution routing, shared queue rejection, generation retirement, and
   deterministic command/worker regressions.
3. `332ef9d6`, `fix(tui): reconcile stop state before choosing pause owner`.
   Stop reconciliation ordering and the minimized event/snapshot regression.
4. `docs(tui): record cleanup review and merge verification`.
   This audit and the corrected backend test command in the preceding audit.

All commits use explicit staging allowlists. The stop-order fix has its own
commit; separating its staging did not modify the tested working files.
Review cleanup alone with `git diff b1fe38a4..HEAD`; review the complete branch
with `git diff dev...HEAD`.
