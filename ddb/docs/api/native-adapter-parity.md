# Native adapter API work

This work continues on `codex/vscode-api-parity` in the persistent worktree
`/mnt/home/ybyan/projs/DDB-vscode-api-parity`. The main DDB checkout is unchanged.
The adapter remains on `codex/canonical-ddb-api` until the backend contracts are ready.

The objective is to remove the adapter's construction of GDB MI commands, not
just send those commands through a different transport.

## Implemented so far

- Empty variable expansion accepts GDB's omitted children collection only when
  `numchild` explicitly equals zero. Malformed responses remain errors.
- ExecuteRawCommand accepts native CLI text and the matching GDB/LLDB CLI dialect.
  DDB handles quoting and conversion to its internal command protocol. A mismatched
  explicit dialect is rejected. The existing MI dialect remains compatible.
- Validation: 322 core unit tests pass, one ignored; all three API integration
  tests pass. Rust client/type, TypeScript and Python SDK suites also pass. The real GDB inspection test
  sets a convenience variable through native CLI and reads it through Evaluate.

- Native console requests now accept a canonical frame_id, using shared ownership
  validation with Evaluate and a pre-dispatch lifetime check. The real GDB test
  reads a caller-frame local and resumes execution through native CLI. The
  generated Rust, TypeScript and Python contracts include frame_id.
- The LLDB bridge uses explicit SBExecutionContext for framed commands, following
  [LLDB's command interpreter API](https://lldb.llvm.org/python_api/lldb.SBCommandInterpreter.html).
  Its frame forwarding is covered without an LLDB installation.

- Variable child projections preserve lazy dynamic expandability and omit an
  inexact child count. Explicitly empty values remain non-expandable. A focused
  decoder-to-projection regression covers dynamic and static metadata, following
  [GDB's variable-object contract](https://www.sourceware.org/gdb/current/onlinedocs/gdb.html/GDB_002fMI-Variable-Objects.html).
  Root metadata enrichment now inspects only the requested local page.
- Framed WATCH/HOVER evaluations return retained, expandable identities and typed
  metadata. The real GDB scenario checks a side-effecting expression runs once
  across repeated expansion, then verifies expiration after stepping. A focused
  ownership test checks bounded allocation, in-flight readers and cleanup on
  thread removal or command failure. Ordinary expansion shares that cleanup
  guard. The Rust/TypeScript/Python SDK suites and codegen drift check pass.
- The full real-backend test attempt passed both GDB scenarios. Its LLDB scenario
  could not run because `lldb` is not installed.

- SetVariable assigns nested local children and retained watch children through
  their opaque identities. The real GDB scenario verifies the resulting memory
  values, read-only rejection, idempotent replay and stale-handle rejection.
  The existing ownership test now checks assignment rejects other sessions and
  threads before dispatch. The LLDB bridge uses SBValue.SetValueFromCString,
  covered by a focused success/rejection test; runtime LLDB remains unavailable.

## Remaining work

1. Finish dynamic/container coverage with a real pretty-printer once typed settings
   are available. Local metadata and typed assignment are implemented.
2. Add typed pretty-printer and source-mapping settings, with corresponding SDK
   contracts and focused validation.
3. Replace the adapter's raw-variable implementation and MI console/setup command
   construction. Verify real GDB inspection, assignment, caller-frame console,
   configuration, lifecycle cleanup and the packaged VS Code extension.

Reuse the existing operation admission, authentication, idempotency, frame guards,
and generated contract machinery. Extend existing integration scenarios where
possible instead of introducing duplicate test harnesses.
