# Runtime architecture

This document defines the ownership and synchronization rules for DDB's in-memory debugger model. Treat these rules as architecture constraints, not implementation suggestions.

Debugger-native ownership and backend extension rules are documented in
[`debugger-backends.md`](debugger-backends.md).

## Composition

`ApplicationServices::build` is the composition root. It creates one `Arc<RuntimeModel>` and gives clones of that facade to command, API, session, source-resolution, and event-reduction services.

`RuntimeModel` exclusively owns these mutable repositories:

- session and thread state
- service-group membership
- breakpoint aggregates
- proclet ownership
- per-group operation gates

The repository modules are private to `state`. Callers cannot obtain a manager handle or a mutable session guard.

## Allowed access

Callers interact with runtime state in one of three ways:

1. Commands on `RuntimeModel` perform a complete domain mutation.
2. Immutable snapshots carry data across an `await` or into an API response.
3. Short-lived read views support latency-sensitive identity projection without copying indexes.

Do not add manager getters to `RuntimeModel`. If a caller needs new behavior, add a domain-named command or immutable query to the facade. A command that spans repositories belongs in `RuntimeModel`, not in the caller.

## Mutation paths

- Debugger notifications flow through `DebuggerEventReducer`, which updates topology and breakpoint state through the model.
- Session admission and removal flow through `SessionActivation`.
- Breakpoint and execution commands flow through their command services and model commands.
- API and diagnostic reads use snapshots; they never reach into repositories.
- Source resolution depends on the narrow `GroupResolutionView` query interface.

This keeps transports and presentation code from becoming alternate owners of domain state.

## Synchronization

The lock order is:

1. group operation gate
2. session metadata lock
3. repository leaf lock

Repository leaf locks must not be held across an `await`.

Operations that can race with group membership changes hold an opaque group-operation guard. Multi-group operations acquire gates in sorted, deduplicated order. Session retirement begins by acquiring the session's group gate and returns a `PendingSessionRetirement`; consuming that token performs all model cleanup and yields a `SessionRetirement` that keeps holding the gate. Callers publish the retirement's breakpoint state changes before dropping it, so retirement records can never interleave with records from a later group operation. Dropping the retirement releases the gate and removes an emptied group's gate entry.

Command services publish their breakpoint records after releasing their gates; that ordering is best-effort between concurrent commands. Only lifecycle transitions (activation's breakpoint synchronization and retirement) publish while holding the gate.

Because activation holds the group gate from membership publication through bootstrap, a command targeting that group waits for the activation to finish instead of failing against a not-yet-routable session. Expect group-command latency to include an in-flight same-group bootstrap.

A `SessionTransaction` owns ordered exclusive session runtime leases and exposes controlled model commands. It does not expose a mutable session-state reference.

## Invariants

The model and its tests must preserve these properties:

- A session becomes group-visible only after activation acquires its gate, and is not routable until group breakpoint synchronization and bootstrap complete.
- Activation and retirement serialize with commands targeting the same group.
- Removing the last session removes its group gate only after model cleanup.
- Retirement removes session, group, breakpoint-target, proclet-owner, and thread-selection state as one coordinated transition.
- Snapshots are detached values and cannot mutate repositories.
- Cross-session transactions acquire session leases in stable numeric order.
- State-manager types remain inaccessible outside the `state` implementation.
- Retirement breakpoint records are published while the retirement still holds the group gate.

## Accepted states

Activation reserves a group before acquiring its gate. If the activation future is dropped between reservation and membership publication (in practice only when admission tasks are aborted during shutdown), an empty reserved group remains. It is inert: group ids are never reused, commands targeting it fail with "no live sessions", and the next same-hash activation adopts it. No sweeper removes it, deliberately — removing an empty group that a waiting activation has already reserved would race that activation's membership publication.

## Change checklist

When adding runtime behavior:

1. Put repository-local mechanics in the relevant private manager.
2. Put cross-repository ordering and invariants in `RuntimeModel`.
3. Return an immutable snapshot or narrow read view.
4. Add a model-level invariant test for lifecycle or concurrency behavior.
5. Run the profile-build and CI gates from the workspace root:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
```

For changes on a command or API hot path, compare the same release benchmark scenarios, scales, warmup, and sample count before and after the change. Evaluate percentile latency rather than a single elapsed-time sample.
