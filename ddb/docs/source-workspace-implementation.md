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

Verification uses canonical operations through the public Rust SDK,
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
- Plan and research committed as `4e8561ad`.
- The first refactor introduces `SourceBrowser`, which owns document
  state, navigation history, following state, and source request generations.
  Source requests have an independent worker task lane. Frame inspection no
  longer reads source or clears the displayed document.
- `cargo check --manifest-path ddb-tui/Cargo.toml --offline` passed after the
  refactor with no warnings. Formatting and `git diff --check` passed.
- Migrated tests from removed App source fields to the browser interface. The
  unit suite passes all 90 tests, including nine browser regressions. Strict
  Clippy with `--all-targets -- -D warnings` passes.
- The mock full-terminal workflow passes with source preservation during running,
  independent cursor and execution movement, and explicit follow resumption.
  Its initial preservation assertion incorrectly included the toolbar play icon;
  the corrected assertion examines the body below the toolbar.
- Document/frame separation was committed as `eeca56ef`.
- Stage 2 is split into a shared-filesystem commit and a canonical API commit.
  The `ddb-source-workspace` module pins explicit directory grants with
  `cap-std`; subsequent reads and enumeration stay relative to those grants.
  It rejects traversal, escaped symlinks, nonregular files, and oversized reads.
- Sixteen shared-filesystem tests pass, including FIFO rejection, cached-file
  replacement, symlink replacement, independent roots, shared byte/count bounds,
  CRLF/blank/EOF handling, 2,001 directory entries, and 100 repeated windows in a
  100,000-line source file. Strict Clippy for this module and the SDK passed.
- Content and directory snapshots have independently bounded shared caches.
  A regression demonstrated that rapid directory edits can share timestamps.
  First-page listings therefore always enumerate afresh; continuation snapshots
  can be reused for at most 30 seconds with renewed access checks. The regression
  now passes without sleeps or weakened fresh-listing assertions.
- Backend source discovery/export implementation and its canonical contract are
  under verification. The serial check passed 333 backend unit tests with one
  pre-existing ignored test, source SDK/security tests, HTTP/gRPC parity, existing
  source resolution, and runtime OpenAPI/AsyncAPI conformance. Real-debugger
  verification is still running; these results do not establish TUI completion.
- Explorer/quick-open UI, local-root handling, frontend ownership-aware markers
  and breakpoints, and complete terminal verification remain unfinished.

### Canonical source API stage

- Shared filesystem commit: `04267e85`. Package documentation and complete
  metadata accompany the API commit.
- Added `ListSources`, `ListSourceRoots`, and `ListSourceEntries` to Protobuf,
  both transports, SDKs, and generated contracts. `SourceFile` identifies its
  origin and session owners; `ReadSource` returns exact total line counts.
  Removed the old path-only source reader rather than retaining a fallback.
- `SourceRoots` exports are explicit and config-relative. Session tags and build
  prefixes select content mappings. Debugger catalogs are session-owned,
  refreshable, globally bounded, and protected against late publication after
  session cancellation. Missing sources remain visible with availability hints.
- Rust catalog collection preserves per-session warnings and refreshes only the
  first page. TUI source loading uses the new content shape and reloads the final
  window for beyond-EOF navigation. This is contract adoption, not explorer UI.
- Verified `cargo test -p ddb --features grpc-preview` with `--bin ddb` and test
  targets `api_v2`, `api_v2_sources`, `api_v2_source_security`, `api_v2_grpc`,
  `api_v2_spec_conformance`, and `source_resolution`: 333 unit tests passed,
  one existing test ignored, and all 11 selected integration tests passed.
- Real GDB and LLDB tests passed for `typed_v2_inspection` and
  `continue_with_running_peer`: four tests. These now assert source discovery,
  source content, and that refresh does not interrupt running sessions.
- TUI unit tests: 91 passed. The three selected `terminal_workflow` PTY tests
  passed for mock, GDB, and LLDB. The other 12 PTY workflows were not run in this
  stage; new explorer/distributed-source workflows are still required.
- Rust SDK/types tests: 20 passed. TypeScript type checks and seven tests passed;
  Python's five tests passed. `ddb-api-codegen --check` reproduces all artifacts.
  The established OpenAPI and AsyncAPI validators passed with 48 HTTP operations
  and two stream channels. `git diff --check` passed.
- Strict Clippy passed for TUI, source-workspace, and the Rust SDK. Backend
  strict Clippy passed with `--all-targets --no-deps` and only
  `-A clippy::cargo_common_metadata`. Unrestricted backend linting still fails
  on existing package metadata and legacy `gdbmi` warnings; no source lint
  suppressions or unrelated dependency refactors were added.
- An earlier overlapping build reported missing compiled extension crates.
  The serial backend verification above rebuilt successfully without code
  workarounds. An initial gRPC test expected the wrong unknown-ID error;
  it now checks the existing `NotFound` contract and passes.
