# Debugger variable scopes survey

Research date: 2026-09-30. This is a source-code survey, not an interactive comparison of installed releases. All external implementation links pin the revisions inspected. Findings describe those revisions and may differ from older installed adapters.

## Conclusion

A separate global-variable browser is an established debugger feature. Excluding globals from a locals scope fixes classification, but leaves browsing incomplete when no separate scope exists. There is no single industry layout. CodeLLDB separates static and global variables; LLVM lldb-dap combines them into Globals; cppdbg exposes neither separately; Delve offers package globals as an option.

The stronger design for DDB is a small locals scope and separately requested globals and file statics, with explicit scope extent and frame context. Watch expressions remain useful for known names, but do not replace discovery. These are recommendations based on the implementations below, not claims that every debugger follows them.

## Comparison

| Adapter | Scope organization | Extent of globals | Loading behavior |
| --- | --- | --- | --- |
| CodeLLDB | Local, Static, Global, Registers | Variables associated with the selected frame's compilation unit, separated by LLDB value type | Enumeration starts on a variables request; all scopes are marked non-expensive |
| LLVM lldb-dap | Locals, Globals, Registers | Globals and statics associated with the selected frame's compilation unit | Globals and registers are expensive; each scope loads on first variables request and caches its values |
| VS Code C/C++ cppdbg / MIEngine | Locals, Registers | No separate global scope in the inspected DAP handler | Scope creation allocates references; register scope is expensive |
| Go Delve | Locals; optional Globals (package ...); optional Registers | Current package | Global display is opt-in; enabled globals are loaded during the scopes request |

The sections below provide sources and qualifications for each row.

## CodeLLDB

The scopes handler returns Local, Static, Global and Registers. Every scope has `expensive: false`. Local requests combine arguments and locals with `statics: false`; Static and Global requests both ask LLDB for statics, then filter by `VariableStatic` or `VariableGlobal`. Enumeration occurs in the variables handler. It does not use DAP `start` and `count` to page these lists. Conversion has cancellation and a time budget, with a visible timeout entry if exceeded.

The converter replaces repeated local names with the later value, but disables that name-based deduplication for Static and Global. Thus this implementation is not evidence that same-named globals should be dropped. The code does not provide a written product rationale for separating Static and Global. The evident mechanism distinguishes LLDB storage categories.

Source: [CodeLLDB scope creation, loading and conversion](https://github.com/vadimcn/codelldb/blob/62def434dc22c1d77d4837c4d99cddef7ac3338f/src/codelldb/src/debug_session/variables.rs#L29).

## LLVM lldb-dap

The adapter creates Locals, Globals and Registers. Only Locals is non-expensive. Each scope stores a frame and loads its variables on demand, once per scope object. Locals includes arguments, locals and a stop return value when applicable. The code explicitly explains why it combines them: VS Code automatically expands only the first non-expensive scope, so separate arguments and return-value scopes would make locals harder to see.

Globals uses `GetVariables(false, false, true, true)`, which includes statics and globals. It pages conversion and responses with `start` and `count`, but first obtains the whole underlying SBValueList. That is not proof of incremental symbol discovery. Name lookup searches backward to prefer the closest scope.

Source: [LLVM scope loading and page conversion](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/tools/lldb-dap/Variables.cpp#L177), [scope creation and expense flags](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/tools/lldb-dap/Variables.cpp#L441).

Duplicate display names receive a declaration location suffix such as `name @ file.cc:42`, or a storage-location suffix if declaration information is unavailable. This preserves distinguishable variables instead of deleting every repeated name. The variable still has a separately generated expression path for evaluation.

Source: [LLVM display-name disambiguation and expression path](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/tools/lldb-dap/JSONUtils.cpp#L251).

Frame selection also controls debugger-console context. The scopes handler selects the corresponding LLDB thread and frame so native console commands inspect the same context as the GUI.

Source: [LLVM scopes request context selection](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/tools/lldb-dap/Handler/ScopesRequestHandler.cpp#L16).

## What LLDB means by frame globals

`SBFrame::GetVariables` does not enumerate every variable in every loaded module. It obtains the frame's variables with file globals enabled. `StackFrame::GetVariableList` adds the selected frame's compilation unit variable list. A compilation unit is typically one source file and its included headers, so it can contain many library constants even when the selected function has few locals.

The API's `statics` option covers global, static and thread-local value categories. It filters lexical scope and removes repeated internal Variable objects by identity. Equal display names alone are not the deduplication criterion. This distinction matters for DDB's reported repeated constants and for legitimate same-named file statics.

Sources: [SBFrame enumeration and identity filtering](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/source/API/SBFrame.cpp#L752), [compilation-unit variables](https://github.com/llvm/llvm-project/blob/a651871a3f3f8aef58ae03d2fa81dcc1481b633c/lldb/source/Target/StackFrame.cpp#L441).

CodeLLDB uses the same SBFrame API pattern. Its Global label therefore should not be interpreted as a process-wide symbol browser. CodeLLDB's separate type filters also differ from LLVM's combined Globals scope, particularly for thread-local values.

## VS Code C/C++ cppdbg / MIEngine

MIEngine's OpenDebugAD7 scopes handler creates Locals and Registers only. Locals is non-expensive; Registers is expensive. The register comment says the scope should remain present without reading every register merely to create it. Variables are retrieved separately through frame-property enumeration; locals use the all-locals-plus-arguments filter.

This supports the earlier observation that DDB's locals-only browsing resembles cppdbg, but does not establish it as the best interface or a universal convention. The previous DDB regression test established local-variable enumeration parity between GDB and LLDB; it did not test or justify the absence of richer scopes. There is no Globals or Statics scope in this handler. That does not mean users cannot evaluate global expressions elsewhere.

Sources: [MIEngine scopes and variable requests](https://github.com/microsoft/MIEngine/blob/fdd455222c3233daf55143b027870639e13f7108/src/OpenDebugAD7/AD7DebugSession.cs#L1941), [MIEngine frame-property enumeration](https://github.com/microsoft/MIEngine/blob/fdd455222c3233daf55143b027870639e13f7108/src/MIDebugEngine/AD7.Impl/AD7StackFrame.cs#L397). The [project README](https://github.com/microsoft/MIEngine) identifies MIEngine as the engine behind cppdbg.

## Go Delve

Delve combines arguments and local variables into Locals. If `showGlobalVariables` is enabled, it adds `Globals (package <name>)`, removes the repeated package prefix from variable labels and retains the package context. Registers are separately optional.

The source explicitly says the package restriction keeps the data manageable and that even this was expensive enough to disable global display by default. It loads package globals during the scopes request. A comment proposes deferring loading until expansion and considers one scope per package. These are comments about potential improvements, not implemented guarantees.

Delve marks shadowed variables with parentheses around their names. Its indexed-variable requests can load a requested slice. That paging support should not be confused with deferred enumeration of package globals.

Source: [Delve scopes implementation and rationale](https://github.com/go-delve/delve/blob/79d454d91cbf8f72028d287cd3a5b02593d06678/service/dap/server.go#L2661), [indexed paging](https://github.com/go-delve/delve/blob/79d454d91cbf8f72028d287cd3a5b02593d06678/service/dap/server.go#L2780), [shadowed-name presentation](https://github.com/go-delve/delve/blob/79d454d91cbf8f72028d287cd3a5b02593d06678/service/dap/server.go#L2975).

## DAP and VS Code presentation

DAP allows adapters to choose scope names. It defines an `expensive` flag for scopes whose variables are large in number or expensive to retrieve, optional variable counts, and variables requests with paging. Client behavior determines when those variables are requested and whether counts and paging are used. It does not define a universal Globals scope, mandate a scope hierarchy or provide a scope-level collapsed flag. Variable references for suspended-state values have a lifetime tied to suspension.

Source: [DAP Scope and related types](https://github.com/microsoft/debug-adapter-protocol/blob/701c28841af3d62977d1cd9ffa8fc5c89a693881/specification.md).

VS Code restores saved tree state, then expands the first non-expensive scope automatically when appropriate. Therefore Locals first, followed by expensive Globals/Statics, is a useful default layout. It cannot guarantee that globals stay collapsed after the user has expanded them previously.

Source: [VS Code variables view](https://github.com/microsoft/vscode/blob/9666ca8ea7a0ac0d0ce4e661c19f8a05d5d7ad79/src/vs/workbench/contrib/debug/browser/variablesView.ts#L87).

## Implications for DDB

The following are design recommendations, not changes made by this survey:

1. Keep **Locals and arguments** first. Preserve function-local statics according to lexical visibility. Do not restore unrelated compilation-unit globals to that list.
2. Add separately requested **File statics** and **Globals**, showing the compilation unit or source file in the scope label or description. Alternatively, a combined **Globals and statics** scope follows LLVM's model and avoids suggesting that every backend can classify all storage categories equally well.
3. Mark costly scopes expensive, create their references without evaluating all values, and load only when requested. Show Registers separately. Do not promise default collapse as permanent UI behavior.
4. Treat a process-wide global search or module browser as a distinct feature. A frame's compilation-unit globals do not cover the entire process, and target-wide enumeration can be much larger.
5. Use session, thread, frame and stop generation to bind scopes and variables. DDB can have several paused sessions at once; the selected distributed frame must supply the correct owner and evaluation context.
6. Distinguish symbol identity from its label. Preserve different declarations and shadowed values; disambiguate with declaration location or module. Remove only verified duplicate identities. Keep display labels separate from expression paths.
7. Carry paging through the SDK and backend. Slicing an already fetched, fully evaluated list reduces UI size but not debugger work. Scope metadata and symbol enumeration should avoid reading every value solely to calculate counts.

The current DDB API already declares scope kinds for arguments, locals, statics, globals and registers, plus expense/count fields and paged variable requests. The current service still creates only the locals scope, while the adapter appends Registers. Existing page responses follow full enumeration, and the adapter collects pages before applying its DAP slice. These are implementation gaps rather than a reason to invent an LLDB-specific UI route. Add browsing through typed ListScopes/ListVariables, with backend-specific enumeration behind the native debugger boundary. The adapter should consume typed scopes rather than issue raw GDB or LLDB commands. Thread-local values require the owning thread as well as the process context. Sources: [DDB scope resource model](../proto/ddb/api/v2/resources.proto), [DDB inspection service](../core/src/api/application/service.rs), and [adapter inspection implementation](/mnt/home/ybyan/projs/vscode-adapter/src/v2/inspection.mts:324).

GDB's Python block API supplies `static_block` for the current compilation unit's static symbols and `global_block` for its global symbols. This provides a native basis for a shared scope contract, although symbol readability, value creation, imported symbols, and paging still need implementation tests. Source: [GDB Blocks in Python](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Blocks-In-Python.html).

## Validation needed before implementation is considered complete

Use a small C++ fixture with arguments, ordinary locals, function-local statics, two file statics sharing a name, namespace globals, thread-local values, included-header constants, shadowing and unavailable optimized values. Verify both GDB and LLDB, then the actual greeter frame. Assert that merely selecting a frame does not eagerly evaluate Globals. Check expansion, editing and Watch expressions against the correct declaration, frame and session; check paging and invalidation after resume. Measure large compilation-unit lists separately from local-variable latency.

This survey does not establish the exact global enumeration strategy for GDB, verify behavior on older installed LLDB versions, or provide UI screenshots from the compared adapters. Those are implementation and validation tasks, not missing evidence for the existence of separate global scopes.

## Revisions inspected

- vadimcn/codelldb: `62def434dc22c1d77d4837c4d99cddef7ac3338f`
- microsoft/MIEngine: `fdd455222c3233daf55143b027870639e13f7108`
- llvm/llvm-project: `a651871a3f3f8aef58ae03d2fa81dcc1481b633c`
- go-delve/delve: `79d454d91cbf8f72028d287cd3a5b02593d06678`
- microsoft/debug-adapter-protocol: `701c28841af3d62977d1cd9ffa8fc5c89a693881`
- microsoft/vscode: `9666ca8ea7a0ac0d0ce4e661c19f8a05d5d7ad79`
