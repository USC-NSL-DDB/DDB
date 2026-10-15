# Stop and logpoint lifecycle audit

This audit covers the complete backend protocol and its client projection,
including the call paths surrounding the stop-policy implementation. The starting
revision is `dd70cbc2`. The implementation history begins at `6e1d07ad`.
The behavior contract is [stop-policies.md](stop-policies.md).

Review status: in progress. Evidence below is an inventory until the final
completion audit records the exact checks and their results.

## Identities and ordering

A native stop belongs to one backend instance, global thread identity, and
native stop revision. A debugger-local thread number can be reused. Public
execution revisions can change during inspection without a new native stop.
Automatic work also belongs to a control epoch and an active task lease.

The relevant stages are native event commitment, classification, task scheduling,
public operation admission, queueing, native-state preparation, synchronous
writer admission, writer acknowledgment, command response, and observed execution
change. A command response and its execution event can arrive in either order.
Preparatory frame queries, evaluations, register restoration, and migrated-heap
cleanup have their own queueing and write-admission boundaries.

Cancellation wins when it advances the epoch or expires the task lease before
the final synchronous admission gate. Already admitted writes can finish after
cancellation. A rejected queue insertion is unadmitted; a later transport failure
does not prove that an admitted write was unsent.

## Invariants

| ID | Required property |
|---|---|
| I1 | A guarded operation addresses the observed backend instance and exact native thread/stop. Session retirement, local-thread reuse, or a newer native stop vetoes stale work. Inspection-only revision changes do not invent another native stop. |
| I2 | Accepted explicit intent retires conflicting automation before the operation becomes visible. Invalid, conflicting, expired, or capacity-rejected requests do not retire work. Idempotent replay does not repeat admission effects. Automatic evaluation and continuation do not cancel themselves. |
| I3 | Every automatic native command, including queued reads and preparatory writes, checks its current stop, epoch, and task lease at final writer admission. Cancellation before that gate admits no later automatic write or successful logpoint output. |
| I4 | Successful final resume admission consumes the process's stopped claims exactly once. Preparatory writes require claim availability without consuming it. Rejected admission leaves claims available. A claimed stop remains consumed after native rejection, as the public contract specifies. |
| I5 | A successful logpoint output record and its final resume share admission. Output rejection admits no resume; writer rejection publishes no success record. Already admitted work can produce an uncertain completion and keeps its claim consumed. |
| I6 | Primary ordinary stops, transparent logpoints, failed logpoints, observed automatic pauses, and secondary process-stop threads have distinct dispositions. A current deferred ordinary stop is not silently forgotten when another cause naturally disappears. Explicit retirement can retire old candidates. Group-stop targets the cause's current logical group. |
| I7 | A native logpoint runs at most once. Failure, saturation, cancellation, and mode changes cannot restart evaluation of the same native stop. A still-current failed or abandoned logpoint remains inspectable and can establish an ordinary cause in the applicable mode. Stale task completion cannot change a newer stop's disposition. |
| I8 | A pause receipt is evidence of an admitted attempt, not an observed stop. Unsent work creates no receipt. Ambiguous failures retain evidence; definite rejection removes only its own contribution. Manual intent and newer independent stops cannot inherit old automatic provenance. |
| I9 | Queued peer pauses validate both the initiating native stop and the exact running peer generation. Retired causes, changed peer generations, and stale attempt results cannot update a newer coordination attempt. Failures stay visible and unchanged peers are not retried indefinitely. |
| I10 | Heap cleanup holds its session lease through native acknowledgment and receipt removal. Failed live cleanup preserves receipts; retired-peer receipts expire. Context restoration expires inspection handles only after successful restoration. |
| I11 | State publication and SDK/TUI hydration converge on backend classification. Observed automatic pauses match exact current execution identity and revision. Pending targets and acknowledgments cannot suppress an independent stop. Backend restart invalidates old guards and client provenance. |
| I12 | Task count, deadlines, rendered output, retained operations, and pause evidence remain bounded. No synchronous leaf lock spans an await or debugger I/O. Native session locks and runtime leases use a consistent order; callbacks do not reverse it. |

## Boundary evidence matrix

Each row needs both a cancellation or supersession check and the relevant failure
check. Existing test names are evidence locations, not assertions that the entire
row has been verified. Missing cases must be added or explicitly resolved by the
implementation audit before this document is marked complete.

| Boundary | Invariants | Evidence and outstanding checks |
|---|---|---|
| Request validation and backend-instance check | I1, I2 | `execution_guards_reject_intervening_stops_and_backend_restarts`; operation-store invalid-key/conflict/capacity tests. Audit rejected requests against pending automation. |
| Accepted operation and idempotent visibility | I2 | `idempotent_read_cannot_observe_admission_before_cancellation_finishes`; `rejected_admission_does_not_run_effects_or_retain_an_operation`. Audit every invalidating command class, including evaluation and assignment. |
| Native stop classification and cause turnover | I6, I9 | `an_ordinary_stop_and_logpoint_in_one_snapshot_are_both_handled`; group-stop tests. Add deferred ordinary-stop turnover after natural retirement. |
| Task scheduling and saturation | I7, I12 | Task limit is 16. Verify saturation disposition, later mode changes, completion reaping, and no duplicate scheduling. |
| Queued frame capture | I1, I3, I7 | Current code checks around frame capture. Verify the native queue boundary itself and malformed/failed frame responses. |
| Queued expression evaluation | I1, I3, I7 | `logpoint_evaluation_cannot_execute_after_an_intervening_stop`; verify explicit cancellation, malformed evaluation, and task deadline. |
| Migrated-heap lease acquisition and cleanup | I3, I4, I10 | `cancelled_migration_cleanup_preserves_heap_records_and_never_writes`; partial-failure and retired-peer tests. |
| Heap acknowledgment and receipt removal | I10, I12 | `migration_cleanup_holds_the_session_lease_through_receipt_removal`; verify a queued new restoration is resampled. |
| Context-restoration queue and acknowledgment | I1, I3, I4, I10 | `cancelled_or_stale_logpoint_does_not_restore_registers_after_queuing`; successful restore/output-once test. Verify native restore rejection and post-restore cancellation. |
| Actor event fence and native-state preparation | I1, I3, I4 | Native actor tests for preceding stops, events arriving during validation, and a busy writer; runtime-model tests for retirement, ID reuse, and consistent source/peer sampling. |
| Cancellation after preparation | I3, I4, I5 | `cancellation_after_validation_prevents_logpoint_output_and_admission`; `cancellation_and_admission_have_a_single_order_across_threads`. Verify both winner orders. |
| Output preflight and writer queue admission | I4, I5 | Add native writer-rejection and closed-output cases. Verify neither output sequence nor claim advances on rejected admission. |
| Writer acknowledgment failure | I4, I5, I8 | Distinguish already admitted failure from failed insertion. Verify output and claims remain committed after late acknowledgment failure. |
| Native command rejection and timeout | I4, I7, I8 | Proven-native-rejection command-port tests; native pending timeout tests. Verify task lease expiration while operation remains queued. |
| Command acknowledgment versus native Running/Stopped | I1, I8, I11 | `continue_acknowledgment_does_not_overwrite_a_newer_native_stop`; native projection watermark tests. |
| Peer pause queue, source retirement, peer restart | I1, I8, I9 | `queued_peer_pause_cannot_interrupt_a_newer_running_generation`; failed-peer-pause tests. Audit stale attempt completion against current policy generation. |
| Late pause and late rejection | I6, I8, I11 | Failed-sent, overlapping-attempt, late-rejection, and persistent-manual-epoch regressions in `stop_policy_tests.rs`. |
| Policy reconfiguration and stale CAS | I2, I6, I7, I8 | `two_policy_writers_using_the_same_version_cannot_overwrite_each_other`; observed automatic/manual mode-change tests. Add queued/failed/saturated logpoint disposition checks. |
| Natural retirement versus explicit cancellation | I2, I6, I7, I9 | Audit retained seen/disposition maps and whether valid deferred stops can take over without rearming explicitly retired work. |
| Snapshot, replay, new clients, and restart | I1, I8, I11 | Two-client backend tests, API client replay test, restart process test, TUI exact-revision/hydration/focus tests. |
| Shutdown and output backpressure | I3, I5, I12 | Actor shutdown transport/projector backpressure tests; output hub gap/closed-subscriber tests. Verify closure ordering with guarded publication. |

## Lock order under review

Public admission takes operation store, policy state, then execution admission.
Native dispatch retains session read locks in session identity order, then takes
task activity, execution admission, process claims, and output publication locks.
Writer insertion is synchronous. Dispatch callbacks acquire policy state only
after releasing the activity, execution, claim, and output locks. Runtime
transaction leases use session identity order and are distinct from native model
locks. Heap cleanup holds one transaction lease at a time.

The final audit must check all call paths against this order, including failure,
timeout, cancellation, and shutdown paths. An ordinary green test is insufficient
evidence for an unexamined transition.

## Completion evidence

Pending. Record confirmed defects, rejected hypotheses, exact regression names,
validation commands/results, and commit identities after the matrix is complete.
