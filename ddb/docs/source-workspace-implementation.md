# Source workspace implementation

Status: in progress. Branch: `codex/tui-next`.

## Requirements

Source browsing is independent of frame inspection. Opening a document must not
select a thread, change the execution target, cancel variable expansion, or
change expression evaluation context. Inspection refreshes must not erase open
documents. Manual navigation disables automatic following until explicitly
restored. Selecting a frame deliberately reveals that frame's document.

DDB exposes debugger-known sources and explicitly configured exported workspace
roots through its canonical v2 contract and SDKs. Workspace browsing works without
a stopped frame, and includes files absent from debug symbols. Missing debugger
sources remain visible. There is no MI, v1, or raw-command frontend fallback.

Source identity includes its origin and ownership. Equal path strings from
different sessions or roots must not imply equal documents. Build paths,
debugger-host mappings, DDB-host exports, and TUI-local checkouts are distinct.
Existing typed debugger source mapping remains the debugger configuration path.
Local workspace files must not become remote breakpoint locations implicitly.

The TUI provides quick-open, lazy directory navigation, keyboard and mouse
interaction, document navigation history, and explicit follow/reveal controls.
Distributed session/group breakpoint scope remains explicit.

Server exports are opt-in. Directory listing and content reads enforce root
containment, including symlinks, and bounded resource use. Source and directory
requests have cancellation/staleness handling independent of frame requests.
File windows reuse bounded content caches instead of repeatedly reading and
hashing whole files. Catalog refresh and pagination have defined invalidation.

## Commit sequence

1. `docs: record source workspace implementation and verification plan`
   Records this plan and the CGDB research. No application behavior changes.
2. `refactor(tui): separate source documents from frame inspection`
   Introduces the source-browser module, independent request lifetime, document
   identity, follow state, and navigation history. Removes replaced App fields
   and frame-owned source loading. Includes navigation behavior tests.
3. `feat(api): expose source catalogs and configured workspace roots`
   Extends the existing source discovery module, configuration, canonical
   Protobuf operations, transport handlers, SDK methods, and generated docs.
   Implements bounded listing/read behavior, ownership, and export containment.
   Includes SDK-visible integration tests and generator drift checks.
4. `feat(tui): add source explorer and quick-open navigation`
   Connects typed discovery to the browser, adds root configuration and local
   checkout handling, directory and quick-open UI, keyboard/mouse actions,
   visible follow state, and source-aware breakpoint behavior.
5. `test: verify distributed source browsing end to end`
   Adds terminal and real-debugger coverage, mapping/isolation regressions,
   performance checks, user documentation, and removes obsolete paths.

Commit boundaries may split further when needed for review. Each implementation
commit records its actual checks here. Generated files accompany their canonical
inputs. Stage explicit paths, never unrelated work. Do not claim end-to-end
completion while any requirement remains unverified.

## Verification interfaces

Proposed for confirmation: canonical operations through the public Rust SDK,
the source-browser module's navigation interface, and rendered keyboard/mouse
workflows through the existing terminal test harness.

Required behavior checks:

- Browse before selecting a thread and while the program runs.
- Stops and inspection refresh preserve a manually chosen document and cursor.
- Explicit frame reveal and return-to-execution display the intended owner.
- Stale source responses cannot replace newer navigation, and source navigation
  does not invalidate variable requests.
- History restores documents and positions without changing debugger selection.
- Duplicate paths across sessions/roots retain distinct identities.
- Debug-source listing includes unavailable files and has stable bounded pages.
- Workspace listing includes non-debug files and does not expose unconfigured
  directories, traversal targets, or symlink escapes.
- Local checkout mapping and debugger-host source mapping remain distinct.
- Breakpoints retain explicit DDB session/group routing.
- Large catalogs, wide directories, and long files have bounded memory/work;
  cancellation and cache invalidation behave correctly.
- HTTP and gRPC contracts agree; generated Rust, TypeScript, Python, OpenAPI,
  and AsyncAPI artifacts reproduce, and existing conformance gates pass.
- Real GDB/LLDB and distributed-session terminal tests verify observable source
  browsing, stepping, marker placement, and breakpoint behavior.

## Evidence log

- Baseline inspected: `370cde8c`, `refactor(tui): align inspection with the current
  v2 SDK`. The only initial untracked file was the preceding CGDB research note.
- Confirmed frame coupling in `goto_source_line`, `prepare_source_navigation`,
  worker inspection cancellation, and `clear_inspection_details`.
- Confirmed existing source discovery in `core/src/source`; current public source
  operations are `ResolveSource` and `ReadSource`, with DDB-host filesystem reads.
