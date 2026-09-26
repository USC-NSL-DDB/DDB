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
- Validation: 319 core unit tests pass, one ignored. The real GDB inspection test
  sets a convenience variable through native CLI and reads it through Evaluate.

## Remaining work

1. Add canonical frame context to native console requests, with ownership and
   stopped-frame lifetime checks. Regenerate the contract and SDK types.
2. Complete variable metadata and dynamic/container expansion. Evaluate must
   provide an expandable result identity; handle lifetime and cleanup belong in
   DDB rather than the client.
3. Add typed variable assignment, including children without a standalone
   assignable expression.
4. Add typed pretty-printer and source-mapping settings.
5. Update Rust, TypeScript and Python client/generated contracts as applicable,
   with focused tests of the changed requests and results.
6. Replace the adapter's raw-variable implementation and MI console/setup command
   construction. Verify real GDB inspection, assignment, caller-frame console,
   configuration, lifecycle cleanup and the packaged VS Code extension.

Reuse the existing operation admission, authentication, idempotency, frame guards,
and generated contract machinery. Extend existing integration scenarios where
possible instead of introducing duplicate test harnesses.
