# DDB and ddb-tui integrated usability readiness

Status: **implemented and verified as a Linux release candidate**

Evaluation date: 2026-08-15

This report is the requirement-to-evidence record for
[the integrated usability implementation plan](ddb-tui-integrated-usability-implementation-plan.md).
It covers the first-party ddb and ddb-tui workflow. The broader public API
contract, SDK, extension, and compatibility evidence remains in the
[community API readiness report](api/release-readiness-2026-08-14.md).

The paired artifact is not eligible to be called an official open-source
release until the repository has a project license. Packaging deliberately
records this as official_release_eligible=false; it does not invent or silently
omit legal metadata.

## Delivered architecture

The implemented shape preserves two independently useful binaries behind one
streamlined workflow:

~~~text
ddb tui <config>
    -> execs sibling ddb-tui
    -> supervises sibling ddb serve <config>
    -> negotiates authenticated public API v2
    -> renders and controls the debugger exclusively through ddb-api-client
~~~

ddb owns configuration, debugger sessions, state, API service, headless
operation, and launch/attach construction. ddb-tui owns terminal behavior and
the managed child lifecycle. The dispatcher adds no second supervisor and there
is no in-process or private debugger path.

The default workflows are:

~~~bash
ddb tui debug.yaml
ddb tui launch --backend gdb -- ./target/debug/app
ddb tui attach --backend gdb --pid 1234
~~~

The direct frontend and externally owned service workflows remain:

~~~bash
ddb-tui debug.yaml
ddb-tui connect --api https://debug.example.com
ddb serve debug.yaml
~~~

Distributed configuration uses the same one-command managed path. connect
expresses external lifecycle ownership; it does not mean that distributed
debugging requires manual backend startup.

## Requirement traceability

| Requirement | Implemented contract | Verification evidence |
|---|---|---|
| UX-01 | Managed config, generated launch, and generated attach work from ddb tui and direct ddb-tui; port, token, and readiness are automatic. | CLI unit tests; managed_mock_one_command_workflow; ddb_tui_dispatcher_runs_the_managed_frontend; managed GDB/LLDB launch and GDB attach PTY tests. |
| UX-02 | ddb serve is independently headless; external connect owns no process; companion resolution is sibling-first with explicit/PATH fallbacks and actionable errors. | managed_serve_is_headless_authenticated_and_reports_actual_endpoint; CLI/resolver tests; paired-artifact empty-PATH smoke test. |
| UX-03 | The TUI depends on public ddb-api-client/ddb-api-types, not DDB core/config modules; all debugger operations and state cross API v2. | Dependency/source audit; all Mock and real-debugger PTY operations; API conformance gates. |
| UX-04 | ddb-tui canonicalizes and forwards configuration without deserializing it; DDB applies configuration and per-session on_exit with the global field as fallback. | CLI mode tests; config/session factory tests; distributed managed PTY test. |
| UX-05 | Managed DDB forces loopback port 0 and file-only control/admin credentials. The credential source is private and unlinked after DDB loads it. | 50-way parallel managed_serve; runtime-file permission tests; managed_credentials_never_cross_process_or_diagnostic_boundaries; /proc argv/environment and log/report/UI sentinel checks. |
| UX-06 | Startup report v1 is bounded, versioned, create-new, and published atomically; failures have stable phase/code; API version, capability, backend identity, and version are negotiated before normal managed operation. | Startup report unit tests; seven managed_serve process tests; supervisor early-exit/timeout tests; compatibility tests; late-connect PTY test. |
| UX-07 | Owned backend shutdown is explicit; launch kills, attach detaches, connect never owns; SIGINT/SIGTERM/SIGHUP, backend crash, terminal loss, startup timeout, and panic paths restore/clean up according to policy. | Supervisor tests; managed launch/attach PTY tests; signal, crash, terminal-loss, and panic PTY tests; lifecycle matrix below. |
| UX-08 | Service readiness is independent of target hydration; a two-session configuration renders the first target while the second is delayed. | managed_distributed_sessions_surface_partial_readiness asserts 1/1 followed by 2/2 API-derived topology. |
| UX-09 | Legacy positional ddb config and ddb-tui --api remain supported; v1 fallback is explicit; dispatcher preserves argument boundaries and exit status. | DDB and TUI CLI characterization tests; explicit_v1_fallback_uses_the_real_legacy_api; argument-boundary and dispatcher PTY tests. |
| UX-10 | Host-specific paired archive contains both binaries, manifest, completions, examples, guide, release notes, architecture, and this report; sibling resolution works with empty PATH; diagnostics include both sides of compatibility. | Reproducible packaging script, checksum, manifest, and extracted-artifact smoke test. |
| UX-11 | Startup diagnostics identify resolution/config/auth/bind/startup/readiness/negotiation phases; backend logs use a bounded in-memory tail plus retained file; secret/source/expression data is excluded. | Structured startup failure tests; bounded supervisor report/log tests; credential sentinel PTY test; user guide recovery table. |

## Bugs found and closed during final verification

The production-readiness pass found and fixed three classes of defect rather
than treating the initial green unit suite as sufficient:

1. The TUI formerly conflated source navigation and execution state. The source
   gutter now uses independent ▶ execution and ▸ cursor markers. Mock, GDB,
   and LLDB tests assert that stepping moves only the execution marker while a
   pinned cursor remains independent.
2. External v2 clients that started before DDB could fail before the recovery
   worker was established. Retryable transport failure is now deferred only for
   externally owned v2 mode; authentication, protocol, version, and managed-mode
   failures remain fail-closed.
3. LLDB can emit its initial stopped event before session bootstrap finishes.
   DDB previously projected this pre-activation session as EXITED and could
   expose thread resources before the command router accepted the session. DDB
   now retains legacy OFF wire compatibility, projects API v2 STARTING,
   withholds pre-activation process/thread resources, and integration helpers
   wait for legacy ON before issuing commands. Two focused lifecycle tests and
   five repeated real LLDB attach runs verify the fix.

## Lifecycle fault-injection matrix

| Scenario | DDB after frontend outcome | Debuggee after outcome | Terminal/runtime files |
|---|---|---|---|
| Managed config, normal quit | Gracefully stopped and reaped | Configuration on_exit policy | Restored; runtime directory removed |
| Generated launch, normal quit | Gracefully stopped and reaped | Killed by default | Restored; runtime directory removed |
| Generated attach, normal quit | Gracefully stopped and reaped | Detached and remains alive | Restored; runtime directory removed |
| External connect, normal quit | Remains running; never receives owned shutdown | Unchanged | Restored; no managed runtime directory |
| Startup failure or timeout | Process group terminated and reaped | No unexpected survivor | Startup files cleaned; terminal restored if entered |
| SIGTERM/SIGINT/SIGHUP | Owned process group stops | Launch/config policy applies | Alternate screen, cursor, mouse, and raw mode restored |
| Backend crash | No managed DDB survivor | Backend-dependent; failure is explicit | TUI remains recoverable, then restores terminal |
| Terminal/PTY loss | Linux parent-death signal stops owned DDB | No unexpected managed survivor | Runtime directory removed |
| Intentional TUI panic | Drop guard kills owned process group | No unexpected managed survivor | Terminal restored and credentials removed |
| Partial distributed target delay/failure | Healthy coordinator remains usable | Healthy targets remain controllable | Partial topology is API-visible |

The verified hardening mechanism is a Unix process group plus Linux
PR_SET_PDEATHSIG. The paired candidate and PTY evidence in this report are
Linux-specific. Other target archives can compile, but equivalent Windows job
object/macOS lifecycle evidence is required before claiming the same orphan
guarantees on those platforms.

## Security assessment

Managed mode applies these controls:

- loopback-only bind with OS-assigned port 0;
- distinct cryptographically random control and admin bearer tokens;
- private 0700 runtime directory and create-new 0600 token document;
- token values absent from arguments, environment, startup report, retained
  backend log, TUI status/output, and diagnostics;
- managed credential file unlinked after DDB loads authorization into memory;
- startup report published create-new, including broken-symlink protection;
- no automatic privilege escalation for ptrace/Yama attach failures;
- control token exposed only to the ordinary TUI client and admin token retained
  only by the supervisor for shutdown; and
- external connect mode never gains managed ownership implicitly.

The sentinel PTY test copies the short-lived token document through a private
test shim before the real DDB exec, then searches /proc, logs, report, and
terminal capture for both exact sentinels. It also proves the original token path
is absent before readiness.

## Verification record

The final local Linux gate uses the following commands.

From ddb/:

~~~bash
cargo fmt --all --check
cargo check --workspace --all-targets --all-features
cargo test --workspace --all-targets
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-api-types --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-api-client --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-api-grpc --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-api-conformance --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-api-extension --all-targets --no-deps -- -D warnings
cargo clippy -p ddb-sample-extension --all-targets --no-deps -- -D warnings
cargo test -p ddb --test managed_serve
~~~

Notable results:

- all DDB workspace unit and integration targets pass;
- managed_serve: 7/7, including 50 concurrent port-zero starts;
- the LLDB attach regression passes once as a focused diagnostic and five
  consecutive repetitions after the fix; and
- strict scoped clippy passes for DDB plus every public API/extension crate.

From ddb-tui/:

~~~bash
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
cargo test --test e2e_mock -- --ignored --test-threads=1
~~~

Results:

- unit/component suite: 51/51;
- PTY suite: 16/16 in 23.86 seconds;
- strict all-target/all-feature clippy: pass; and
- optimized release build: pass.

Artifact and performance gates:

~~~bash
ddb/tools/package-ddb-tui-release.sh /tmp/ddb-paired-release-final
ddb/tools/test-ddb-tui-release.sh <archive>
ddb/tools/measure-tui-dispatch.sh ddb/target/release/ddb \
  ddb/benchmarks/evidence/2026-08-15-tui-dispatch/result.json 100
~~~

The dispatcher evidence is retained under
[benchmarks/evidence/2026-08-15-tui-dispatch](../benchmarks/evidence/2026-08-15-tui-dispatch/).
Its recorded p95 overhead is 5.809694 ms against the 100 ms gate.

CI mirrors the scoped lint policy, full workspace tests, all PTY workflows,
managed lifecycle tests, reproducible paired packaging, empty-PATH artifact
smoke test, and dispatcher p95 gate.

## Gate deviations and non-goals

The following are explicit, not hidden:

- cargo clippy --workspace --all-targets --all-features -- -D warnings still
  reports 43 findings in the legacy gdbmi/dependency-metadata lint surface. This
  implementation does not claim that command passes and does not refactor
  unrelated legacy code. CI uses strict --no-deps lint gates for DDB and each
  maintained public API/extension crate, plus a full strict TUI lint.
- --keep-backend is deliberately unavailable. Persistence was gated on durable
  credential ownership, token rotation, reconnect metadata, and cleanup; leaking
  a child/runtime directory was explicitly rejected.
- The archive is a release candidate while LICENSE is absent. The manifest
  records no license files and official_release_eligible=false.
- Linux lifecycle and real GDB/LLDB behavior are verified. Cross-platform process
  ownership requires platform-specific job/liveness implementation and tests.
- Language-SDK conformance and long API soak targets remain their existing
  explicit/CI gates; they were not silently converted into ordinary fast tests.

## Artifact contents and audit evidence

The release script produces a byte-for-byte reproducible gzip archive and a
companion SHA-256 file. The extracted smoke runs with an empty PATH and proves:

~~~text
bin/ddb tui --help
bin/ddb serve examples/managed/mock.yaml
bin/ddb tui examples/managed/mock.yaml
bin/ddb-tui --ddb-path bin/ddb examples/managed/mock.yaml
~~~

The archive manifest records both binary versions, host target, supported API
range, binary paths, license files, and official-release eligibility. The CI
artifact retains the archive, checksum, and dispatch benchmark JSON together.

## Auditable commit ledger

The implementation was separated from the originally mixed working tree with allowlisted path staging and cached-diff review. The review series is:

1. `feat(api): establish the public DDB v2 platform` — backend runtime, v2 contract, generated artifacts, SDKs, extensions, examples, and API/core tests.
2. `feat(tui): add the first-party DDB-native frontend` — the independent TUI crate, managed launch, DDB-native interaction model, and mock/real PTY tests.
3. `test(api): add release gates and performance evidence` — CI, fuzzing, reproducible API package checks, benchmarks, and retained evidence.
4. `docs(ddb): publish native frontend and community contracts` — plans, architecture, user guide, release notes, and readiness ledgers.
5. `build(release): package the paired DDB binaries` — paired reproducible archive and extracted empty-`PATH` smoke test.

Each commit body records Requirements, Why, Compatibility, and Tests. Use `git log --format=fuller` and `git show --stat` as the authoritative hash and path ledger. No remote branch was updated while preparing the series.

## Release decision

The implementation and Linux technical gates are ready for a paired release
candidate. The remaining blocker for an official open-source release is a
maintainer-selected project license. Platform owners must additionally supply
equivalent lifecycle evidence before advertising non-Linux orphan-safety parity.
