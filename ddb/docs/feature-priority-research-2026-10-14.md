# DDB feature-priority research

Research date: 2026-10-14. Repository baseline: `6e1d07ad` on `dev`. These notes support a feature audit, not an implementation commitment. They do not claim workload measurements or results from new application runs.

These historical notes support the [feature-priority assessment](ddb-feature-priorities-2026-10-14.md). Repository observations refer to the baseline above; linked source files may have changed since then. Backend-owned stop policies and logpoint continuation were implemented afterward. See the [stop and logpoint lifecycle audit](api/stop-logpoint-lifecycle.md) for that follow-up. Other candidates have not been reassessed here.

## Existing capabilities to preserve

DDB already had a public Protobuf contract, HTTP/ProtoJSON and SDKs, distributed backtraces, group/session breakpoints, source discovery, managed launch/attach, and extension descriptors at this baseline. The TUI already defaulted to distributed stacks and provided local file browsing, conditions, watches, memory/register inspection, and logpoints. See [the project README](../../README.md), [TUI guide](../../ddb-tui/README.md), and [v2 guide](api/v2.md). Recommending these as missing would misrepresent the assessed revision.

## Candidates with the strongest evidence

### Backend-owned stop policies and logpoints

[The TUI guide](../../ddb-tui/README.md#logpoints-and-value-inspection) explicitly says logpoint metadata belongs to the TUI, is not persisted, and is invisible to other clients except as ordinary breakpoints. It also documents separate stop validation and Continue calls. [Preconditions](../proto/ddb/api/v2/common.proto) at this baseline offered resource/state revisions, not an expected server instance or execution-stop identity. The existing [merge audit](ddb-tui-merge-readiness-2026-10-13.md#contract-limits) explains the restart limitation.

Recommendation: backend-owned bounded log actions, one policy shared by clients, and admission/dispatch execution guards. Decide how explicit user control wins over pending automatic actions. Reject changed instances and changed stops. Failure must retain the stop. Persistent policy should store logical definitions, never old runtime handles.

GDB supports dynamic printf without recompilation and offers debugger, inferior-call, and agent styles. This supports the concept, not a universal overhead guarantee. DDB should advertise backend-specific support and measure before selecting native versus stop/evaluate/continue implementations. [GDB dynamic printf](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Dynamic-Printf.html).

### Request-centric causal inspection

[Distributed backtrace collection](../core/src/cmd_flow/backtrace.rs) follows one ancestor chain. Parent capture failures break collection and log an error. Static inspection found no visited-set or traversal-depth guard in that loop; this is not a reproduced hang. The API frame cap is applied after collection, so a bounded result alone does not establish bounded traversal. [The result contract](../proto/ddb/api/v2/resources.proto) describes ordered frames and truncation, not a request graph with explicit missing-edge causes and observed-versus-reconstructed provenance.

Recommendation: start by reporting partial-capture causes and frame provenance, then add request identities and bounded fanout/fanin relationships. A read-only graph should not imply that historical reconstructed frames are executable or that independently sampled processes form an atomic snapshot.

OpenTelemetry propagation correlates work across service boundaries. Its span API supports links to other span contexts, including relationships outside a parent-child tree. These concepts inform DDB's data model. They do not prove that telemetry spans provide native stack reconstruction, memory inspection, or complete causality. [Context propagation](https://opentelemetry.io/docs/concepts/context-propagation/), [span links](https://opentelemetry.io/docs/specs/otel/trace/api/#specifying-links).

### Secure, resilient distributed attachment

[The existing Kubernetes producer](../core/src/discovery/k8s_producer.rs) disables certificate validation, fixes the namespace to `default`, selects ServiceWeaver labels, discovers a PID with `ps`, handles Added events, ignores Modified/Deleted events, and stops watching after stream termination. This is an existing integration to improve, not a missing Kubernetes feature.

Recommendation: verified endpoint identities, explicit namespace/container selection, stable workload/process identities, relist/reconnect, retirement on replacement, and stage-specific attach diagnostics. Do not silently weaken host or cluster security to make attachment succeed.

Kubernetes documents ephemeral debug containers for images without utilities. Runtime support determines whether a targeted container's process namespace is visible. Consequently, a distroless deployment workflow needs explicit capabilities and a supported process-discovery strategy; a `ps`-based implementation alone is insufficient. [Kubernetes running-Pod debugging](https://kubernetes.io/docs/tasks/debug/debug-application/debug-running-pod/).

### Portable diagnostic bundles

The [v2 guide](api/v2.md) describes bounded, expiring operation records rather than an audit log, and separate state/output streams. Existing topology JSON export is useful but is not a complete offline investigation.

Recommendation: opt-in, versioned bundles containing selected state, distributed frames and provenance, operation outcomes, bounded output, build/source identifiers, collection times, and redaction manifests. Provide offline read-only browsing. Such a bundle cannot resume execution or reevaluate expressions for which the required values were not captured. Never promise restored executable state from a bundle of debugger reads.

Deterministic replay is a separate, much larger feature. rr documents single-core execution, recording-tree shared-memory limits, CPU requirements, and syscall support limits. It does not establish coordinated multi-host replay. Those constraints argue for a useful diagnostic capture before a general reverse-debugging investment. [rr limitations](https://rr-project.org/#limitations).

### Public framework/instrumentation integration SDK

The [public API extension SDK](api/extension-authoring.md) already supports namespaced state and actions. It is different from a framework integration contract. [FrameworkPlugin](../core/src/plugin/mod.rs) at this baseline referred to core `Config`, native protocol values, and `RuntimeModel`. Community frontend SDKs alone do not let a new messaging framework supply discovery and causal metadata independently.

Recommendation: define a narrow integration contract for process identity, discovery lifecycle, causal links, capability declarations, and bootstrap hooks. Ship an independent sample integration and conformance tests. Avoid arbitrary in-process dynamic Rust libraries and direct mutable-model access. An external companion protocol may be preferable, but that choice needs a separate design and threat review.

DAP demonstrates the benefit of reusable debugger adapters, capability negotiation, and explicit stopped-handle lifetimes. It is evidence for public integration boundaries, not proof that DAP expresses DDB group rules or distributed context reconstruction. Keep DDB-native semantics in the canonical API. [DAP overview](https://microsoft.github.io/debug-adapter-protocol/overview.html).

## Alternatives and limits

- Typed data watchpoints are a legitimate next-tier candidate. The schema names a watchpoint stop reason, but that does not establish typed creation/management. GDB documents hardware resource/width limits and much slower software watchpoints, with multithread limitations. Require explicit capability reporting and no silent software fallback. [GDB watchpoints](https://www.sourceware.org/gdb/current/onlinedocs/gdb.html/Set-Watchpoints.html).
- A wait-for or deadlock view needs instrumented wait ownership and synchronization evidence. Sampled stacks alone cannot prove a distributed deadlock. A causal graph and trustworthy partial-data reporting should precede such claims.
- A new frontend or binary transport alone does not address the concrete lifecycle and shared-semantics gaps above. HTTP/ProtoJSON and optional gRPC already exist.
- Priorities are engineering judgments based on the repository and official specifications. No customer demand study or performance benchmark was available in this research.
