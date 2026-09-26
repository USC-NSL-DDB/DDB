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
- Validation: 320 core unit tests pass, one ignored; all three API integration
  tests pass. Rust client/type, TypeScript and Python SDK suites also pass. The real GDB inspection test
  sets a convenience variable through native CLI and reads it through Evaluate.

- Native console requests now accept a canonical frame_id, using shared ownership
  validation with Evaluate and a pre-dispatch lifetime check. The real GDB test
  reads a caller-frame local and resumes execution through native CLI. The
  generated Rust, TypeScript and Python contracts include frame_id.
- The LLDB bridge uses explicit SBExecutionContext for framed commands, following
  [LLDB's command interpreter API](https://lldb.llvm.org/python_api/lldb.SBCommandInterpreter.html).
  Its frame forwarding is covered without an LLDB installation.

## Remaining work

1. Complete variable metadata and dynamic/container expansion. Evaluate must
   provide an expandable result identity; handle lifetime and cleanup belong in
   DDB rather than the client.
2. Add typed variable assignment, including children without a standalone
   assignable expression.
3. Add typed pretty-printer and source-mapping settings.
4. Update Rust, TypeScript and Python client/generated contracts as applicable,
   with focused tests of the changed requests and results.
5. Replace the adapter's raw-variable implementation and MI console/setup command
   construction. Verify real GDB inspection, assignment, caller-frame console,
   configuration, lifecycle cleanup and the packaged VS Code extension.

Reuse the existing operation admission, authentication, idempotency, frame guards,
and generated contract machinery. Extend existing integration scenarios where
possible instead of introducing duplicate test harnesses.
