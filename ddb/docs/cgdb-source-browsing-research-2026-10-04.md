# CGDB source browsing and implications for DDB

Research date: 2026-10-04. This is a documentation and source-code survey, not an interactive test of CGDB. The manual describes release 0.8.0. Implementation findings pin upstream revision [dd6d76d609c79f97c44d3f5f4145069648f9b8d3](https://github.com/cgdb/cgdb/commit/dd6d76d609c79f97c44d3f5f4145069648f9b8d3).

## Conclusion

CGDB supports browsing source independently of the selected stack frame. Its file picker is a searchable list of debugger-known files, not a general directory explorer. DDB should retain that distinction when adding workspace browsing. A directory tree and a debug-symbol source catalog answer different questions, particularly when sessions run on different hosts.

The recommendations below concern a proposed design. This survey does not claim they are already implemented in DDB.

## Documented interaction

In the source pane, `o` opens a full-screen file dialog. Arrow or vi-style keys navigate; `/` and `?` search with regular expressions; Enter selects a file; `q` cancels. Source navigation supports line jumps, paging, search, local and cross-file marks, and a last-jump location. The `'.` command returns to the execution location. The cursor and execution line have separately configurable visual indicators. Source-pane breakpoint commands act on the cursor line. Debugger stepping moves the source view to the new stop location.

The manual describes a symbol-derived file list and an error when no source files are available. It does not document a general filesystem explorer or a browser-style multi-entry back/forward history. Its jump-back command should not be mistaken for such a history.

Sources: [CGDB source window](https://cgdb.github.io/docs/cgdb.html#Source-Window), [CGDB mode](https://cgdb.github.io/docs/cgdb.html#CGDB-Mode), [file dialog](https://cgdb.github.io/docs/cgdb.html#File-Dialog-Mode).

## Implementation findings

These points follow from the inspected code rather than an interactive run.

### File discovery is separate from frame inspection

Pressing `o` queues `tgdb_request_inferiors_source_files`. The backend sends GDB MI `-file-list-exec-source-files`, then converts each result into a pathname, preferring `fullname` over `file`. It does not derive the choices from the current stack or recursively scan a project directory.

Sources: [source-pane key handler](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/interface.cpp#L1261), [source result conversion](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/lib/tgdb/tgdb.cpp#L263), [MI request](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/lib/tgdb/tgdb.cpp#L1312).

GDB documents this operation as returning files it knows about. The absolute pathname can be absent, and reading additional debug information can reveal more files. Consequently, this catalog is neither proof that content is accessible nor an exhaustive listing of a checkout.

Source: [GDB MI file commands](https://sourceware.org/gdb/current/onlinedocs/gdb.html/GDB_002fMI-File-Commands.html#The-_002dfile_002dlist_002dexec_002dsource_002dfiles-Command).

### Choosing a file does not select a frame

The dialog calls `if_show_file(path, 0, 0)`. Zero values leave the stored cursor and execution line unchanged. The source viewer stores separate displayed-file and executing-file pointers. A mark jump changes the displayed file and cursor; the return-to-execution command uses the executing-file pointer. Debugger location updates separately call the same rendering function with the debugger's line.

Sources: [dialog selection](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/interface.cpp#L1566), [separate viewer state](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/sources.h#L44), [line updates](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/sources.cpp#L1158), [mark jumps](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/sources.cpp#L671), [debugger location update](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/cgdb.cpp#L505).

### Local accessibility is a limitation

CGDB adds the debugger-provided paths and already-created disassembly buffers to its picker. The picker removes exact duplicate path strings and rejects ordinary files that fail a local existence check. Loading source calls local `fopen`; this path does not fetch source content from a remote debugger target. Thus missing or remote-only files can disappear from the picker. This is an implementation limitation, not a pattern DDB should copy.

Sources: [picker population](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/cgdb.cpp#L577), [existence filtering and deduplication](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/filedlg.cpp#L123), [local content loading](https://github.com/cgdb/cgdb/blob/dd6d76d609c79f97c44d3f5f4145069648f9b8d3/cgdb/sources.cpp#L210).

## Source directories and path mapping

GDB's source search path is not a project root or a recursive file index. It starts with the compilation directory and debugger working directory. The `directory` command adds search directories; it does not recursively search their descendants. `set substitute-path` rewrites a build-time directory prefix for relocated source trees. Rules match at directory boundaries, and the first matching rule wins. A substitution can override an existing original path, whereas ordinary search paths are used when the original cannot be found.

These mechanisms resolve symbol paths. They do not establish that the TUI, DDB service, debugger process and target share a filesystem.

Source: [GDB source-path rules](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Source-Path.html).

## Recommendations for DDB

These are design recommendations drawn from the distinction above.

1. Model displayed document, cursor and navigation history separately from selected session/thread/frame and actual stop locations. Opening a file must not change variable evaluation context.
2. Provide a fast debugger-source picker and a workspace explorer. Label whether an entry is debug-known, workspace-only, generated or unavailable. Keep missing debug sources visible with a resolution action.
3. Configure named workspace roots explicitly. Treat the launch working directory as an optional initial suggestion, not the universal source root. Support several roots and per-session source mappings.
4. Keep build-time source identity separate from resolved content location. Carry session or target ownership so equal paths on different hosts or revisions do not merge accidentally. A local checkout path must not become a remote breakpoint location merely because it was displayed.
5. Read debugger-owned source through DDB's typed API. Local workspace files can use an explicitly local provider. Do not guess locality from whether a pathname happens to exist on the TUI host.
6. Make following stops an explicit UI state. Manual browsing should preserve the chosen document until the user follows execution or selects a frame. A visible return-to-stop action should work across files.
7. Load trees lazily, bound file discovery and content reads, cancel stale navigation requests, and keep selection stable during refresh. Exclude build outputs by default where appropriate, with a visible way to include them.

A file explorer therefore needs a source-ownership and resolution contract before its layout is finalized. Copying CGDB's picker alone would improve browsing, but would not solve DDB's distributed-source problem.
