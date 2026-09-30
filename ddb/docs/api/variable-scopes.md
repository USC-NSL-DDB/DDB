# Variable scopes

`DebuggerService.ListScopes` describes three frame-owned scopes for GDB and
LLDB. It performs no native variable enumeration or value reads.

| Kind | Name | Contents | Expensive |
| --- | --- | --- | --- |
| LOCALS | Locals and arguments | Arguments, active lexical locals, function-local statics | false |
| STATICS | File statics | File-scope static declarations in the current compilation unit | true |
| GLOBALS | Globals (current source unit) | Global and thread-local declarations in that compilation unit | true |

A compilation unit is a source file and its included headers. This is not a
process-wide global-symbol search. Registers retain the separate typed
`ListRegisters` operation. The VS Code adapter presents them as a fourth,
expensive scope.

## Loading and identity

`ListVariables` enumerates nonlocal declarations only when the scope is
requested. It reads/formats values only for the requested page. Discovering
names can still require traversing the compilation unit's debug information.
An additional metadata-only record determines whether a continuation exists.
Continuation tokens are bound to the scope and execution revision.

Repeated declarations are identified using native identity or qualified name,
source origin, type and storage category within the compilation unit. Equal
names alone are not duplicates. Distinct declarations with equal names receive
source-location suffixes. Display labels are independent of Watch expressions
and opaque variable IDs.

Nonlocal variable IDs retain the owning frame, scope and declaration ordinal.
Expansion and assignment recreate native variable objects from that declaration,
not its display label. GDB returns the symbol's native lvalue through a debugger
convenience function; LLDB retains its SBValue. Both preserve the original
storage. Existing stop-revision checks reject reads/edits after execution changes
and apply equally to distributed caller frames.

Watch expressions use GDB source-qualified names or LLDB native expression paths.
LLDB 20 can incorrectly resolve `::name` to a shadowing local, including through
its expression evaluator. The LLDB bridge resolves exact qualified variable
paths from native declarations first. Compound expressions still use LLDB's
expression evaluator and inherit its language/debug-info limitations.

## Native backend contract

These commands are private implementation details behind the typed service.
They are not SDK methods or an adapter raw-command requirement.

- `-ddb-list-scope-variables --thread T --frame F KIND START COUNT` returns a
  deterministic list of declaration records. KIND is `statics` or `globals`.
  COUNT includes one metadata-only continuation record. The first COUNT minus
  one records include type, value, child metadata and an optional
  `evaluate-name`. Each backend must avoid value/address evaluation for records
  outside that page. Missing debug information produces an empty scope.
- `-ddb-scope-var-create --thread T --frame F NAME KIND ORDINAL` creates a native
  variable object for that exact declaration, with the same result and lifecycle
  as ordinary variable-object creation. Declaration ordering remains stable for
  a stopped frame. Values that cannot be read remain visible as unavailable.

A new debugger backend must implement these native operations before advertising
these scopes. Public scope kinds and the typed inspection/control operations
already exist in API v2; no protocol or SDK version change is required.

## VS Code behavior

The adapter preserves expense flags and scope hints. Globals and File statics
are not automatically evaluated on ordinary frame selection. VS Code may restore
an expanded scope, in which case it requests values normally.

Bounded DAP variable requests stop when their requested range is available.
Without a count, DAP requests the complete collection, which the adapter gathers
through bounded API pages. The existing 10,000-entry collection limit applies;
this does not claim that VS Code virtualizes all named-variable lists. Previously
fetched identities remain available for editing when pages are requested out of
order. Invalid, oversized or non-progressing page requests fail explicitly.

## Validation

The real-backend scope fixture covers lexical/static/global separation, included
header constants, same-named file statics in different compilation units,
shadowing, a thread-local value, one-entry pages, object expansion/assignment,
Watch expressions and expiration after Continue. The packaged greeter UI test
checks all four scopes and browses both nonlocal collections for GDB and LLDB.
Existing integration tests continue to cover optimized/inlined stack frames and
distributed caller inspection. This scope fixture does not assert the behavior
of every compiler's optimized-out global representation.
