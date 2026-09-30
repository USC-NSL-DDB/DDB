# Native adapter API work

The VS Code adapter uses typed API v2 operations for inspection, variable
assignment, debugger settings and execution. Native console requests keep the
debugger's CLI syntax; DDB performs its internal protocol conversion.

GDB and LLDB expose separate locals, file statics and compilation-unit globals.
See [variable scopes](variable-scopes.md) for loading, identity, Watch and paging
semantics.

The implementation includes GDB frame-filter isolation for local inspection and
managed debugger cleanup. See [the GDB hang analysis](../gdb-frame-filter-inspection-hang.md)
for the upstream cause of filtered variable inspection hanging.

The entries below record implementation and validation checkpoints from the
migration. Commit IDs, test counts and package hashes describe those checkpoints,
not the current build. Reproduce validation from the adapter's test instructions.

## Implementation checkpoints

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

- ConfigureDebugger supplies typed pretty-printer enablement and source mappings.
  A real GDB printer test covers dynamic roots, nested lazy children, pagination,
  and assignment without expression names. Source mappings preserve spaces;
  thread-scoped settings and control characters in paths are rejected.
- Backend validation: core/HTTP/gRPC and Rust client/type tests pass, both real
  GDB scenarios pass, TypeScript/Python SDK and LLDB bridge checks pass, and
  generated artifacts reproduce exactly. LLDB runtime remains unavailable.

## Adapter migration and final validation

The adapter migration is committed as `8695795` on `codex/canonical-ddb-api` in
`/mnt/home/ybyan/projs/vscode-adapter`. It vendors the SDK built from backend API
commit `10f0f8aa`. RawVariables and the adapter's MI command construction are
removed. Console/autorun text uses the native CLI dialect with canonical frame
identities; inspection, assignment and settings use typed SDK methods.

Adapter validation passed 113 unit tests and all 12 canonical integration
scenarios. The extracted VSIX passed four stdio scenarios and the real VS Code
extension-host scenario. Its active runtime was compared byte-for-byte with the
committed checkout. The validated package and receipt are saved outside `/tmp`
as `ddb-native-api.vsix` and `ddb-native-api-validation.txt` in the adapter repo.
The VSIX SHA-256 is
`1f187b882c097b1fa51f84e50f5546e89739f3b446b8eb96e3bf05c2070191cd`.

Real runtime coverage uses GDB. LLDB bridge checks pass, but no LLDB runtime is
installed. This initial validation used the existing DDB mock topology for distributed stacks.

Reuse the existing operation admission, authentication, idempotency, frame guards,
and generated contract machinery. Extend existing integration scenarios where
possible instead of introducing duplicate test harnesses.

## Greeter manual-testing regressions

Follow-up work stays on the same backend and adapter branches. The test
application is the instrumented gRPC helloworld client and server in
`/mnt/home/ybyan/codebase/grpc/examples/cpp/helloworld`.

GDB now retains `Conf.on_exit` as the `ddb-on-exit` setting. If DDB exits before
sending cleanup commands, command-stream EOF still applies the configured kill
or detach policy. The Python regression attaches real GDB to a disposable
process and covers both policies while stopped and running:

```sh
python3 ddb/core/tests/gdb_exit_policy.py
```

A managed DDB server also observes its launcher's lifetime and requests graceful
shutdown when the launcher exits. The managed-serve integration test kills a
launcher and verifies its DDB child exits. Unmanaged API servers keep their
existing lifetime.

The adapter follow-up fixes lazy source retrieval, breakpoint ownership and
focus, and distributed caller inspection after traversal interrupts the caller.
Its opt-in greeter extension-host test checks client and server breakpoint
highlighting, rendered caller-frame selection, local and caller variables,
breakpoint-panel source navigation, and inferior cleanup. Direct greeter tests
also verify cleanup after SIGTERM and SIGKILL of DDB. First-stack requests in
that fixture fell from 4.1–4.9 seconds to 25–73 milliseconds after source lookup
moved out of stack rendering. These timings describe this local test setup.

Backend validation for this follow-up: 322 core tests passed, one ignored;
eight managed-serve tests passed; both real GDB API tests passed; all four GDB
EOF/policy combinations passed. No SDK or protocol changes were required.
The latest VSIX hash, commits and packaged test results are in the adapter's
`ddb-native-api-validation.txt` receipt.

## Evaluation resource lifetime

Scalar watch and hover evaluations release their temporary backend object after
reading its value and metadata. They return no expandable variable identity.
A real GDB regression refreshes a scalar watch 1,025 times at one stop, exceeding
the 1,024-root limit without consuming retained capacity.

Expandable evaluations retain their original value so expanding a side-effecting
expression does not execute it again. Those identities expire when execution
changes. The shared stop-scoped root limit still applies to distinct retained
expandable values; this change does not introduce an eviction or release API.
