# Stop and logpoint lifecycle audit

This audit covers the complete backend protocol and its client projection,
including the call paths surrounding the stop-policy implementation. The starting
revision is `dd70cbc2`. The implementation history begins at `6e1d07ad`.
The behavior contract is [stop-policies.md](stop-policies.md).

Review status: complete through `9f69333f`, with terminal validation in
`3a3eba74`.
The matrix records deterministic regressions and implementation checks, not a
formal proof over every possible schedule.

## Identities and ordering

A native stop belongs to one backend instance, global thread identity, and
native stop revision. A debugger-local thread number can be reused. Public
execution revisions can change during inspection without a new native stop.
Automatic work also belongs to a control epoch and an active task lease.

Native stop observations receive their control epoch when they enter
`RuntimeModel`, before awaiting model ownership. Explicit retirement records a
floor for those observations. An older, still-unclassified stop cannot acquire
the new epoch and start automatic work. This stamp does not describe transport
arrival or debugger event time.

The relevant stages are native observation, model commitment, classification, task scheduling,
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
| I2 | Accepted explicit intent retires conflicting automation, including observed but unclassified stops, before the operation becomes visible. Invalid, conflicting, expired, or capacity-rejected requests do not retire work. Idempotent replay does not repeat admission effects. Automatic evaluation and continuation do not cancel themselves. |
| I3 | Every stop-sensitive automatic native command, including queued reads and preparatory writes, checks its current stop, epoch, and task lease at final writer admission. Reads also bind to the inspection context and reject a reconstructed context for their owner. Cancellation before that gate admits no later automatic execution or successful logpoint output. |
| I4 | Successful final resume admission consumes the process's stopped claims exactly once. Preparatory writes require claim availability without consuming it. Rejected admission leaves claims available. A claimed stop remains consumed after native rejection, as the public contract specifies. |
| I5 | A successful logpoint output record and its final resume share admission. Output rejection admits no resume; writer rejection publishes no success record. Already admitted work can produce an uncertain completion and keeps its claim consumed. |
| I6 | Primary ordinary stops, transparent logpoints, failed logpoints, observed automatic pauses, and secondary process-stop threads have distinct dispositions. A current deferred ordinary stop is not silently forgotten when another cause naturally disappears. Explicit retirement can retire old candidates. Group-stop targets the cause's current logical group. |
| I7 | A native logpoint runs at most once. Failure, saturation, cancellation, and mode changes cannot restart evaluation of the same native stop. A still-current failed or abandoned logpoint remains inspectable and can establish an ordinary cause in the applicable mode. Stale task completion cannot change a newer stop's disposition. |
| I8 | A pause receipt is evidence of an admitted attempt, not an observed stop. Unsent work creates no receipt. Ambiguous failures retain evidence; definite rejection removes only its own contribution. Manual intent and newer independent stops cannot inherit old automatic provenance. Later resume or raw control supersedes pending manual pauses only on its resolved sessions; observed manual provenance remains bound to its exact native stop. |
| I9 | Queued peer pauses validate both the initiating native stop and the exact running peer generation. Retired causes, changed peer generations, and stale attempt results cannot update a newer coordination attempt. Failures stay visible and unchanged peers are not retried indefinitely. |
| I10 | Heap cleanup holds its session lease through native acknowledgment and receipt removal. Failed live cleanup preserves receipts; retired-peer receipts expire. Successful context entry and restoration expire the affected owner's inspection handles without changing native stop identity or consuming its resume claim. Failed restoration preserves inspection handles. |
| I11 | State publication and SDK/TUI hydration converge on backend classification. Observed automatic pauses match exact current execution identity and revision. Pending targets and acknowledgments cannot suppress an independent stop. Backend restart invalidates old guards and client provenance. |
| I12 | Task count, deadlines, rendered output, retained operations, and pause evidence remain bounded. No synchronous leaf lock spans an await or debugger I/O. Native session locks and runtime leases use a consistent order; callbacks do not reverse it. |

## Boundary evidence matrix

Each row records cancellation or supersession coverage and the relevant failure
check. Test names are abbreviated only by omitting their Rust module paths.

| Boundary | Invariants | Evidence |
|---|---|---|
| Request validation and backend-instance check | I1, I2 | `execution_guards_reject_intervening_stops_and_backend_restarts`; operation-store invalid-key/conflict/capacity tests. The admission callback runs after those checks and record-size validation. |
| Accepted operation and idempotent visibility | I2 | `idempotent_read_cannot_observe_admission_before_cancellation_finishes`; `rejected_admission_does_not_run_effects_or_retain_an_operation`; `invalidating_commands_cancel_queued_logpoints_before_admission_returns`; `public_repl_evaluation_retires_automation_at_admission`. |
| Delayed typed and legacy dispatch | I2 | `admitted_command_preserves_automation_for_a_later_native_stop`; typed/automatic/legacy engine opcode tests; diagnostic session resume and inspection tests. Metadata survives diagnostic ingress, so acceptance effects run once. |
| Native observation, ownership wait, and classification | I2, I6, I8 | `native_stop_observation_keeps_its_epoch_while_waiting_for_state_ownership`; `explicit_intent_retires_a_logpoint_before_worker_classification`; ordinary-stop, fresh-stop, mode-change, and unclassified automatic-provenance regressions. Retirement publishes its floor before the newer epoch. |
| Native stop classification and cause turnover | I6, I9 | `an_ordinary_stop_and_logpoint_in_one_snapshot_are_both_handled`; `natural_cause_retirement_promotes_an_already_observed_ordinary_stop`; group-stop late-member/independent-group coverage. |
| Task scheduling and saturation | I7, I12 | `mode_change_coordinates_a_saturated_logpoint_without_restarting_it`; `stale_logpoint_failure_cannot_replace_a_newer_transparent_disposition`. The worker reaps its bounded JoinSet before reconciliation. |
| Queued frame capture | I1, I3, I7 | `queued_logpoint_frame_reads_are_fenced_at_dispatch` covers cancellation, newer stop, and consumed claim; `failed_or_malformed_logpoint_frames_remain_inspectable_and_coordinate_peers` covers transport and decode failure. |
| Queued expression evaluation | I1, I3, I7 | `logpoint_evaluation_cannot_execute_after_an_intervening_stop`; `logpoint_evaluation_rejects_a_consumed_stop_before_running_is_observed`; `queued_logpoint_evaluation_rejects_an_intervening_inspection_context`; invalid evaluation leaves an ordinary cause. |
| Context changed before classification | I1, I3, I7, I10 | `inspection_context_entered_before_classification_prevents_logpoint_evaluation` rejects reads from a reconstructed context even if classification sampled its newer inspection revision. Peer reads and native continuation preparation remain valid. |
| Migrated-heap lease acquisition and cleanup | I3, I4, I10 | `cancelled_migration_cleanup_preserves_heap_records_and_never_writes`; partial-failure and retired-peer tests. |
| Heap acknowledgment and receipt removal | I10, I12 | `migration_cleanup_holds_the_session_lease_through_receipt_removal` queues a later restoration and verifies its resampled receipt set. Live partial failure retains receipts; absent/OFF peers expire them. |
| Context entry, restoration queue, acknowledgment | I1, I3, I4, I10 | `context_entry_expires_only_its_owners_reads_and_preserves_native_claims`; `cancelled_or_stale_logpoint_does_not_restore_registers_after_queuing`; `restore_rejection_and_cancellation_after_admission_prevent_logpoint_resume`; successful restoration/output-once test. |
| Actor event fence and native-state preparation | I1, I3, I4 | Native actor tests for preceding stops, events arriving during validation, and a busy writer; runtime-model tests for retirement, ID reuse, and consistent source/peer sampling. |
| Cancellation after preparation | I3, I4, I5 | `cancellation_after_validation_prevents_logpoint_output_and_admission`; `cancellation_and_admission_have_a_single_order_across_threads` exercises admission-first and rejects the retired epoch afterward. |
| Output preflight and writer queue admission | I4, I5 | `writer_rejection_does_not_publish_logpoint_output_or_consume_the_stop`; `closed_output_prevents_native_logpoint_admission_and_keeps_the_stop_available`; `configured_output_overflow_prevents_native_logpoint_admission`; output sequence-exhaustion test. |
| Writer acknowledgment failure | I4, I5, I8 | `late_writer_failure_keeps_admitted_logpoint_output_and_consumed_stop`; `lost_continuation_ack_preserves_output_and_claim_but_coordinates_current_stop`. An uncertain admitted command never re-evaluates the same logpoint. |
| Native command rejection and task timeout | I4, I7, I8 | Proven-native-rejection command-port tests; `expired_logpoint_leases_veto_queued_frames_evaluations_and_continuations` advances the actual 30-second deadline while work remains queued. |
| Native preparation, queue, and projection deadlines | I3, I12 | Blocked guard/fence and queue-insertion deadline tests; `native_command_deadline_bounds_state_projection_and_releases_its_permit`; canceled/closed/shutdown projection-barrier tests retain registration until terminal cleanup. |
| Saturated native event processing | I3, I12 | `native_event_backpressure_faults_at_the_pending_deadline`; `guarded_event_drain_backpressure_faults_before_any_native_write`. A timed-out partial decoder faults the session; it never resumes using consumed decoder state. |
| Command acknowledgment versus native Running/Stopped | I1, I8, I11 | `continue_acknowledgment_does_not_overwrite_a_newer_native_stop`; native projection watermark tests. |
| Peer pause queue, source retirement, peer restart | I1, I8, I9 | `queued_peer_pause_cannot_interrupt_a_newer_running_generation`; failed-peer-pause tests; runtime source/peer identity guards. One pending target prevents overlapping result ownership; callbacks match source identity, native revision, and epoch before updating current outcomes. |
| Late pause and late rejection | I6, I8, I11 | Failed-sent, overlapping-attempt, late-rejection, and persistent-manual-epoch regressions in `stop_policy_tests.rs`. |
| Pending manual pause and later explicit control | I2, I8 | `accepted_resume_and_raw_intent_supersede_only_their_targets_pending_pause` checks fresh logpoints after same-target supersession and preserves unrelated pauses. Control and legacy-engine tests cover target resolution failure and preservation during plain evaluation or breakpoint invalidation. |
| Policy reconfiguration and stale CAS | I2, I6, I7, I8 | `two_policy_writers_using_the_same_version_cannot_overwrite_each_other`; observed automatic/manual mode-change tests; queued, failed, and saturated logpoint mode-change regressions. |
| Natural retirement versus explicit cancellation | I2, I6, I7, I9 | Natural cause turnover regression; `explicit_retirement_does_not_rearm_an_already_observed_ordinary_stop`. `policy_cause_projection_tracks_inspection_revisions_without_native_turnover` refreshes public identity without inventing a native stop. |
| Snapshot, replay, new clients, and restart | I1, I8, I11 | Two-client backend tests, API client replay test, restart process test, TUI exact-revision/hydration/focus tests. |
| Shutdown and output backpressure | I3, I5, I12 | Actor shutdown transport/projector backpressure tests; output hub gap/closed-subscriber tests; `output_shutdown_and_writer_admission_share_one_order`. Slow output subscribers report loss without blocking native admission. |
| Bounded policy publication failure | I7, I9, I12 | `policy_publication_overflow_does_not_abandon_reserved_peer_tasks` uses a valid 1 KiB journal and 18 peers. Scheduling continues even when the full policy event cannot fit. |

## Lock order

Public admission takes operation store, policy state, then execution admission.
Native dispatch retains session read locks in session identity order, then takes
task activity, execution admission, process claims, and output publication locks.
Writer insertion is synchronous. Dispatch callbacks acquire policy state only
after releasing the activity, execution, claim, and output locks. Runtime
transaction leases use session identity order and are distinct from native model
locks. Heap cleanup holds one transaction lease at a time.

The completion review checked success, rejection, timeout, cancellation, and
shutdown paths against this order. Context entry and restoration invalidate
inspection revisions inside the existing session write lock. Read permits inspect
their already-owned native state; they never reacquire a model read while holding
that permit. Native terminal cleanup owns the registration and command permit
through the projection watermark or deadline, and releases them before notifying
the waiter.

Variable-object deletion is separate from stop-sensitive automation. It addresses
a unique backend object name and can finish after a stop expires; its five-second
cleanup deadline does not authorize evaluation or continuation of another stop.

## Limits and rejected hypotheses

The contract intentionally permits writes admitted before cancellation to finish,
and keeps a consumed resume claim after native rejection or an uncertain transport
failure. Those outcomes do not authorize a second automatic continuation.

An event larger than the configured replay journal cannot be published. The
overflow regression verifies that this refusal does not abandon reserved backend
work. Snapshots remain authoritative; deployments need enough journal bytes for
the complete singleton policy event if they require that event lane to carry it.

Decoder timeouts use the nearest pending command deadline sampled at entry to a
transport batch. A partially consumed timed-out batch faults the session and
cancels its projector; continuing the decoder would lose records. Processing
without a pending command has no command deadline. Fault teardown does not join
legacy presentation work, since an in-progress stdout syscall cannot be preempted.
Deadlines and cooperative aborts assume executor progress.

The stale peer-result hypothesis was rejected. A pending target prevents another
attempt for that peer within one cause; callbacks match source global identity,
native revision, and control epoch before touching current outcomes. Separate
pause receipts remain necessary for uncertain admitted attempts across causes.
Group membership is committed during session activation and rechecked by policy
reconciliation; retirement vetoes dispatch through native session status.

The identical-stop public-guard hypothesis was rejected. The coordinated state
mutation expires every affected inspection revision on every native STOPPED
observation, including identical consecutive stops. The existing public guard
regression covers this without an intervening Running observation.

The final independent Standards review found no remaining confirmed violation of
admission ordering, target resolution, bounded ownership, or lock order. The
final Spec review found no remaining confirmed gap in the stop/logpoint lifecycle
contract. These conclusions describe the reviewed transitions and regression
coverage; they do not claim that arbitrary schedules have been exhaustively
verified.

## Completion evidence

The audit found defects at several separate boundaries. The original patch-local
reviews did not establish lifecycle-wide correctness. The matrix now covers
observations that have not yet been classified as well as work already queued.

| Commit | Confirmed defect and resulting change |
|---|---|
| `cebb4d89` | Writer refusal could leave successful logpoint output without an admitted continuation. Output publication now commits with writer admission. |
| `c463cc7b` | Explicit evaluation or delayed dispatch could cancel automation too late or cancel again after a newer stop. Accepted intent retires work exactly once before operation visibility. |
| `64644590` | Natural cause turnover could lose a deferred ordinary stop, and reconfiguration or stale task completion could rearm or overwrite a logpoint disposition. The backend retains each current native disposition and deferred cause. |
| `f04b7f3f` | Output truncation could admit continuation without the complete successful logpoint record. Oversized output now rejects admission before the native write. |
| `2a27d84d` | Queued frame/expression reads could survive cancellation, claim consumption, or inspection changes. Native reads now validate the exact stop, inspection revision, epoch, and task lease. |
| `8267011d` | Command timeouts could stop covering preparation, writer insertion, or state projection and retain command ownership indefinitely. The absolute native deadline now covers dispatch and projection cleanup. |
| `9f69333f` | Delayed classification could assign an old stop a newer control epoch, or evaluate in an already reconstructed context. Native observation epochs, explicit retirement floors, owner-context read checks, and scoped pending-pause supersession close those admission gaps. |
| `3a3eba74` | The real terminal test expected an obsolete status message and hidden output, and LLDB startup inherited an external symbol-server URL. The fixture now follows backend state, opens Output explicitly, and disables inherited debuginfod URLs. |

The final boundary regressions reproduced two additional failures before the
fix: `explicit_intent_retires_a_logpoint_before_worker_classification` observed
successful automatic output after accepted explicit intent;
`inspection_context_entered_before_classification_prevents_logpoint_evaluation`
observed evaluation against a reconstructed context. Native ingress stamps and
the explicit retirement floor address the first. The owner-context read veto
addresses the second. Scoped pending-pause supersession also prevents an older
unobserved Pause from claiming a fresh logpoint after a later resume, while
preserving unrelated sessions' manual requests.

The late-rejection regression distinguishes natural cause turnover from explicit
retirement. Both correct an unsupported automatic-pause classification. Natural
turnover can promote the independent peer stop; explicit retirement cannot use a
pre-intent stop to restart coordination. Its earlier expectation combined those
different transitions and conflicted with the cancellation invariant.

Final validation ran on the frozen code before committing. Commands below run
from `ddb` unless a different directory is stated.

| Command | Result |
|---|---|
| `cargo test -j 2 -p ddb --all-targets --all-features --quiet` | Passed 549 tests across 29 binaries: 466 unit tests and 83 process integration tests. Five optional tests remain ignored. Includes two-client native GDB/LLDB logpoints, intervening stops, backend restart, failed peer pauses, native dispatch, and projection coverage. |
| `cargo clippy -j 2 -p ddb --all-targets --all-features --no-deps -- -D warnings` | Passed after the final backend fix. |
| `cargo fmt --all -- --check` and explicit `rustfmt --edition 2021 --check` for included guard/prerequisite/policy test files | Passed. The included test files require a separate formatter check. |
| `npm run --prefix docs-site check` | Passed the OpenAPI and AsyncAPI contract checks after the final backend fix. |
| `cargo test -j 2 --all-targets --quiet`, from `ddb-tui` | Passed 376 unit and 5 integration tests; 38 environment-dependent tests ignored by default. |
| `cargo test -j 2 --test e2e_mock real_logpoint_terminal_workflow -- --ignored --exact --nocapture`, from `ddb-tui` | Passed both real GDB and LLDB workflows without shell overrides after the fixture repair. |
| `cargo clippy -j 2 --all-targets --no-deps -- -D warnings` and `cargo fmt --all -- --check`, from `ddb-tui` | Passed after the fixture repair. |

Broader validation at `8267011d` passed the default workspace all-targets suite,
workspace all-targets/all-features compilation, the remaining packages' feature
suites, SDK checks, the TUI release build, and `./tools/check-api-release.sh`. The API
release dry run passed reproducible Rust, TypeScript, and Python packaging and
published no artifacts. The later backend changes are private to DDB and have
the complete backend checks above; no public schema changed.

The workspace all-features test run could not pass five optional `gdbmi` RR
tests because the `rr` executable is absent. Their process launch failed with
OS error 2. The other 28 `gdbmi` tests and the default workspace suite passed.
This audit does not claim RR validation.

The terminal LLDB startup failure reproduced twice with the inherited
`DEBUGINFOD_URLS`. Clearing that variable made the workflow pass, and the
committed fixture now clears it in the child environment. Backend integration
fixtures already used this setting.
