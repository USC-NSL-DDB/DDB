# DDB feature priorities

Date: 2026-10-14. Baseline: local `dev` and `origin/dev` at
`6e1d07adcd27b33a08cf1895bf4e474cf3b2e2dd`.

This document preserves the assessment at that baseline. Repository observations
and rankings below describe that revision. Source links point to paths that may
have changed since the assessment.

Follow-up on 2026-10-15: backend-instance and stop-specific guards, distributed
stop policies, and logpoint continuation from rank 1 have since been implemented.
See the [stop and logpoint lifecycle audit](api/stop-logpoint-lifecycle.md) for
invariants, regression coverage, and verification limits. The remaining priorities
have not been reassessed here.

## Scope and confidence

This is a repository-wide feature-priority assessment, not a line-by-line
security audit or a new runtime certification. It examines the public Protobuf
contract, application service, distributed traversal and reconstruction,
framework plugins, connector metadata, discovery and SSH transports, SDKs,
TUI behavior, CI definitions, retained benchmark evidence, and recent audits.
Historical documents are supporting evidence, not assumed current behavior.
The accompanying [primary-source research note](feature-priority-research-2026-10-14.md)
records comparisons with OpenTelemetry, GDB, Kubernetes, rr and DAP.
The assessment did not run a debuggee or change runtime code or configuration.

Recommendations below are engineering judgments. There is no user-adoption
telemetry, incident-frequency data, or fresh performance experiment supporting
a numerical return-on-investment estimate. Relative effort describes breadth,
not a calendar commitment. Static findings need focused regression tests before
implementation claims or security conclusions.

The assessed baseline already has typed v2 APIs, three SDKs, revisioned state
recovery, bounded output, operation receipts, extension descriptors, managed
one-command startup, local launch/attach, distributed-default stacks, group and
session breakpoints, independent source browsing, scopes, watches, assignment,
registers, memory, and native GDB/LLDB coverage. Rebuilding those is not a useful
roadmap. See [the TUI guide](../../ddb-tui/README.md),
[API v2](api/v2.md), and [the CI definition](../../.github/workflows/rust-check.yml).

## Ranked investments

| Rank | Feature | Primary benefit | Relative effort | Confidence |
| --- | --- | --- | --- | --- |
| 1 | Backend-owned distributed stop and breakpoint policy | Consistent safe behavior for every frontend and headless use | Medium to large | High |
| 2 | Request-centric causal inspection with explicit completeness and provenance | Explain cross-process behavior, including missing evidence | Large, with a smaller first milestone | High for the first milestone; medium for broad async coverage |
| 3 | Secure, resilient remote discovery and session lifecycle | Make distributed debugging usable outside a controlled local fixture | Large, with urgent bounded trust fixes | High |
| 4 | Public framework and instrumentation adapter SDK | Let community integrations add real distributed behavior without core forks | Large | Medium to high |
| 5 | Portable offline diagnostic bundles | Preserve and share a distributed debugging investigation | Medium to large | Medium |

Value ranking is not a strict implementation sequence. The SSH and Kubernetes
trust corrections in rank 3 are prerequisites for wider remote deployment and
should not wait for ranks 1 and 2 to finish.

## 1. Backend-owned distributed stop and breakpoint policy

### Evidence and user problem

[The TUI stop handler](../../ddb-tui/src/main.rs) implements `ordinary_stop`
and `pause_running_peers`. All-stop orchestration is therefore a frontend
behavior rather than a single backend policy shared by all clients.
[The logpoint guide](../../ddb-tui/README.md#logpoints-and-value-inspection)
states that logpoint metadata belongs to the TUI process, is not persisted,
and appears as an ordinary breakpoint to other frontends. Revalidation and
Continue are separate calls.

[Preconditions](../proto/ddb/api/v2/common.proto) cover resource and global
state revisions at admission. They have no expected backend-instance identity
or stop identity. [The merge audit](ddb-tui-merge-readiness-2026-10-13.md#contract-limits)
records the remaining restart fence limitation. Existing frame guards and
revision validation are valuable, but they do not provide every execution
operation with an atomic stop-specific dispatch guard.

### Proposed behavior

- Make all-stop, group-stop, and explicitly selected independent execution
  backend-owned policies. Publish the initiating stop, affected targets,
  pending pauses, failures, and policy generation through the public contract.
- Represent logpoints as typed logical breakpoint behavior, including template,
  bounded evaluation, output attribution, and a fail-closed policy. Share this
  behavior with SDK clients and headless DDB, using existing group inheritance.
- Add expected server-instance and target stop/execution identity where needed.
  Validate at the execution boundary under the existing session serialization,
  not only before a request enters an asynchronous queue. A global revision
  alone is too broad for unrelated sessions changing concurrently.
- Define multi-client conflicts. Two clients must not independently resume the
  same stop or fight over policy ownership. Logpoint automation must never
  resume a later unrelated stop. User execution must have a documented way to
  retire pending automatic work.

Backend ownership does not imply simultaneously freezing every machine. Report
per-target observation and failure; do not call it an atomic distributed snapshot.
Logpoints still interrupt execution unless a backend explicitly supports a
non-stopping mechanism. Do not market them as zero-overhead tracepoints.

### First milestone and quality gate

Start with instance/stop fences and one shared stop-policy resource, then move
logpoint behavior. Reuse the current operation, event, and session machinery;
remove the superseded TUI orchestration once native tests establish parity.
Test with no TUI attached, two independent SDK clients, queued requests across
a restart, an intervening stop, late resume acknowledgments, peer pause failure,
and group membership changes. Real GDB and LLDB tests must verify actual stops,
output and continuation, not just stored policy fields. Measure stop-to-peer-
pause latency at increasing session counts and keep partial failures visible.

## 2. Request-centric causal inspection

### Evidence and user problem

[DistributedBacktraceResult](../proto/ddb/api/v2/resources.proto) is an ordered
leaf-to-ancestor frame list with boundaries and truncation metadata. It has no
request graph or per-segment capture provenance. Existing connector metadata
in [backtrace.hpp](../../connector/include/ddb/backtrace.hpp) carries caller
process/thread information and saved register context.

In [DistributedBacktraceService::execute](../core/src/cmd_flow/backtrace.rs),
parent capture and append failures log an error and break, after which the
collected stack returns a successful response. The application
[result projection](../core/src/api/application/mutations.rs) marks truncation
when the collected list exceeds `max_frames`. It does not carry those parent
failure reasons. The traversal loop has no visible cycle/depth bound, while
the public frame bound is applied after collection. This is static evidence
of a completeness/resource-control gap, not a reproduced infinite traversal.

### Proposed behavior

- First make the existing distributed stack honest and bounded. Return typed
  partial-capture reasons, unresolved ancestors, context provenance and limits.
  Enforce traversal budgets, cancellation checks and cycle detection while
  capturing, rather than only trimming the final result.
- Distinguish a live stopped frame, a reconstructed caller context, and a
  trace-only historical relationship. Describe which variables and memory are
  actually inspectable and whether referenced memory is current or captured.
- Add a request/correlation model with stable causal nodes and edges. Support
  fan-out, retries and asynchronous handoffs when integrations supply evidence.
  Keep the current stack as a useful projection of that model.
- Add TUI request selection and causal tree navigation, with links to the
  canonical owning frame and source. Display missing capture evidence rather
  than inventing a complete call chain.

[OpenTelemetry traces](https://opentelemetry.io/docs/concepts/signals/traces/)
provide span context, parent relationships and links. That makes them a useful
correlation input. The engineering inference is to combine that metadata with
DDB's inspectable contexts, not to replace native capture with trace records.
Trace relationships do not supply register or heap snapshots. Timestamp order
across machines is not sufficient proof of causality or deadlock.

### First milestone and quality gate

Ship completeness, provenance and in-traversal bounds before the graph UI.
Use a three-service fixture with fan-out and retry, missing parent, restarted
parent, malformed/cyclic metadata, and reconstruction failure. Verify partial
results remain useful and correctly labeled. Test that normal execution
restores reconstructed contexts and expires old handles. Measure capture
latency and retained bytes against depth and fan-out. Full distributed stepping
across asynchronous boundaries is a later design, not an automatic consequence
of displaying an edge.

## 3. Secure, resilient remote discovery and lifecycle

### Evidence and user problem

[Kubernetes discovery](../core/src/discovery/k8s_producer.rs) already exists.
It disables API certificate validation, fixes namespace to `default`, uses a
ServiceWeaver-specific selector, discovers PIDs through `ps`, processes Added
events, ignores Modified/Deleted events, and logs watch termination without a
relist/reconnect loop. Both the [direct SSH handler](../core/src/connection/ssh_client.rs)
and [proxy SSH handler](../core/src/connection/ssh_client_channel.rs) accept
unverified server keys. These are current implementation observations.

[ServiceInfo](../core/src/discovery/discovery_message_producer.rs) identifies
services through IPv4, tag and PID. A generalized remote lifecycle needs stronger
process-incarnation/container identity. [SessionProcess](../core/src/session/process.rs)
explicitly rejects remote binary launch, although remote attach exists.
The [API security guide](api/security.md) protects the frontend-to-DDB boundary;
it does not remove trust problems on DDB-to-target connections.

### Proposed behavior

- Verify SSH known-host keys for both direct and bastion connections and honor
  the Kubernetes CA configuration. Reject unknown or changed trust by default;
  any development override must be explicit and conspicuous.
- Reconcile list/watch state with resource versions, backoff, relisting after
  expiration, and deletion/restart handling. Make discovery health visible in
  the API and TUI instead of leaving an apparently healthy but inactive system.
- Configure namespace, selectors and container identity. Handle PID reuse and
  process/container restarts without assigning stale controls or breakpoints
  to an unrelated incarnation. Reapply intended logical group rules only after
  admitting and validating a new session.
- Provide a backend-owned preflight/doctor workflow for ptrace policy, debugger
  availability, symbols, paths, trust and discovery connectivity. Offer an
  explicit attach method for images lacking `ps` or debugger tools.
- Preserve one-command managed operation. Make remote launch a separately
  authorized operation with exact arguments and cleanup policy if it is added;
  do not let reconnection implicitly launch or kill applications.

[Kubernetes API concepts](https://kubernetes.io/docs/reference/using-api/api-concepts/)
document resource-version-based watches and recovery after expired history.
Use those semantics instead of treating one watch stream as permanent.

### First milestone and quality gate

Land trust enforcement and regression tests first, then discovery reconciliation.
Test trusted, unknown and changed host keys on direct and proxy hops; invalid
cluster CA; watch disconnect and expiration; pod deletion/recreation; PID reuse;
and permissions failure. Add a disposable multi-container or cluster integration
tier. Local greeter tests do not prove remote trust, namespaces or reconnection.
No test should disable host security globally or terminate unrelated processes.

## 4. Public framework and instrumentation adapter SDK

### Evidence and user problem

The existing [extension provider contract](api/extension-authoring.md) already
supports namespaced state, actions and generic presentation. Rust/TypeScript/
Python clients already consume the public debugger API. Neither should be
described as missing.

However, [FrameworkPlugin](../core/src/plugin/mod.rs) is inside core and uses
core configuration and runtime model types. [Plugin selection](../core/src/plugin/builtin.rs)
matches a fixed framework enum. The current command adapter mostly supplies a
native backtrace command and process identity extraction. UI extension authors
and framework-instrumentation authors therefore have different capabilities.

### Proposed behavior

- Define a versioned public integration contract for discovery identity,
  instrumentation capabilities, causal metadata, context capture and supported
  restoration behavior. Keep state ownership and privileged mutation in DDB.
- Extract a small supported adapter interface and conformance harness. A
  provider should not need a private RuntimeModel reference or knowledge of
  frontend rendering internals.
- Prove the design with one existing framework moved behind the contract and
  one independently built integration. A new integration should not require
  editing the built-in enum/resolver or TUI source.
- Choose trusted static registration for the first supported implementation if
  that meets deployment needs. Design an out-of-process protocol when isolation
  or independent installation requires it. Avoid an arbitrary Rust `.so` ABI.
- Publish connector packaging, supported architecture/compiler constraints and
  disabled-mode overhead expectations. Context capture is not portable merely
  because its metadata schema is portable.

### First milestone and quality gate

Start with discovery and causal metadata, not a universal language debugger.
Run contract tests for malformed/version-skewed metadata, duplicate identities,
timeouts, provider failure and unsupported reconstruction. Validate provider
isolation and limits if using subprocesses. Existing integrations must preserve
their native tests. Measure both instrumentation-disabled and enabled overhead.
Keep ordinary frontend SDKs and framework adapters distinct in documentation.

## 5. Portable offline diagnostic bundles

### Evidence and user problem

Current snapshot and replay APIs recover live debugger state. Operations and
events have bounded retention. [Watch persistence](../../ddb-tui/src/watch_store.rs)
stores expressions, while topology details can be exported. None of those is
a self-contained, portable distributed investigation with captured values and
source identity. The distinction matters when a failing process disappears or
another developer needs to examine the same evidence.

### Proposed behavior

- Add an explicit capture manifest with backend/build identities, per-session
  execution revisions, capture times and consistency limitations, causal stack
  evidence, source checksums and selected values.
- Let users choose bounded outputs, registers, memory ranges and source files.
  Default to excluding sensitive arbitrary memory and credentials. Support
  review/redaction before sharing and private file permissions.
- Add read-only offline TUI inspection through a separate captured-data
  provider. Show uncaptured values as unavailable. Do not send captured live
  frame handles back to a running backend or silently evaluate expressions.
- Use a versioned, size-limited format with integrity checks and safe parsing.
  Do not execute bundled commands, printer scripts or extension code.

This is evidence playback, not deterministic program replay. GDB's
[process record/replay](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Process-Record-and-Replay.html)
has its own recording modes and target constraints. Coordinating deterministic
replay across networking, scheduling and multiple processes is a separate,
much larger project. Existing proclet reconstruction is not that guarantee.

### First milestone and quality gate

Export one stopped distributed investigation and open it with no backend or
original source checkout. Prove source checksums, partial-capture labels,
missing values and redaction survive round-trip. Reject oversized/corrupt and
path-traversal inputs. Ensure offline mode exposes no execution or evaluation
commands. Capture is opt-in and bounded so it cannot stall normal controls or
silently create a large sensitive artifact.

## Alternatives considered

- Data watchpoints are useful and currently absent from the typed breakpoint
  specification. Hardware code breakpoints are not data watchpoints. They rank
  below the five above for DDB's distributed/backend/community direction, but
  would move up if users primarily investigate memory corruption. Native slot,
  expression-lifetime and backend capability constraints need their own design.
- A wait-for/deadlock view is attractive after the causal model, but RPC ancestry
  alone does not prove lock ownership or a wait cycle. Require runtime evidence
  and clearly distinguish suspected stalls from proven deadlocks.
- Full reverse distributed execution has much higher prerequisites than
  read-only bundles. Do not present a native reverse command or event replay as
  deterministic rollback of a distributed application.
- Making gRPC mandatory or adding another transport is not a top-five feature.
  The repository already has both transports and
  [retained comparisons](../benchmarks/evidence/2026-08-14-v2-transport/README.md).
  Re-run end-to-end workloads before changing transport policy. Old synthetic
  snapshot timings do not establish current production bottlenecks.
- Another frontend or more TUI layout customization offers less leverage until
  shared distributed semantics are backend-owned. Existing UI and SDK work
  should be reused, not replaced.

## Implementation and audit expectations

For each selected feature, first agree on domain semantics and permission
requirements, then change the canonical Protobuf contract, application service,
generated docs/SDKs and first-party TUI as needed. Extend the existing conformance,
native fixture and benchmark suites. Do not implement a second MI compatibility
route or a frontend-only equivalent of new shared backend behavior.

Keep commits reviewable by separating the minimal failing regressions, contract
and generated artifacts, backend implementation, SDK/TUI consumption, and
documentation/verification receipts where practical. Every receipt must identify
the tested revision, environment, exact commands, failures and known limits.
Before removing an old path, demonstrate equivalent behavior through the new
public contract and delete its unused implementation and tests deliberately.

Immediate maintenance should remain outside the feature ranking. For example,
the root [README](../../README.md) says the project has no LICENSE, but a tracked
Apache-2.0 [LICENSE](../../LICENSE) exists and the packaging script checks for it.
That stale documentation is a small correction, not a new product initiative.

At the assessed baseline, the first recommended design was backend instance/stop
fences plus shared stop policy. Remote trust corrections were a concurrent
prerequisite for broader deployment.
