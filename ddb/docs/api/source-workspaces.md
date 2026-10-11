# Source discovery and workspace exports

## Source-relevant breakpoint targets

`DebuggerService.ResolveSourceTargets` accepts an exact debugger source path and
returns only sessions with proven catalog membership, plus their groups. Both
IDE adapters and terminal frontends should use this operation instead of looping
over groups with `ResolveSource` or inferring membership from path suffixes.
Frontend-local path mappings must be applied before the request.

Discovery does not require source content to be readable or a line to be
executable. An optional target restricts discovery. Group results retain normal
breakpoint inheritance; `matchingSessionCount` counts proven matches only.

Stopped sessions use bounded, cached debugger source discovery. Running sessions
use cached membership and are never interrupted. Unknown membership or discovery
failures produce warnings with `complete: false`. An incomplete empty result is
not evidence that the file has no owners. `refresh: true` refreshes stopped
catalogs only. Results contain no broadcast scope and have a 10,000-entry limit.
Authentication, read permission, and request deadlines apply to HTTP and gRPC.

DDB distinguishes debugger-known sources from configured workspace files.
Neither requires selecting a stack frame. Source reads do not select a thread,
change an evaluation context, or move execution.

## Configure exported directories

Add `SourceRoots` to the DDB startup YAML:

```yaml
SourceRoots:
  - name: application
    path: ./checkout
    debugger_path: /build/application
    session_tags: [api, worker]
```

`path` is on the DDB host, relative to the configuration file's directory unless
absolute. DDB opens the directory at startup and fails startup if it cannot be
opened. The root must therefore exist before starting DDB. Root names are unique.
API responses expose the name and an opaque root reference, not this host path.
No parent directory is implicitly exported because it contains a debug source.

`debugger_path` optionally maps a build/debugger path prefix to this directory
for DDB content reads. It does not modify GDB/LLDB settings. Use the existing
typed debugger-configuration operation to change debugger-host source mappings.
A remote frontend's checkout is a third filesystem and is not implied by either
setting.

`session_tags` optionally limits the mapping to named sessions. An empty list
applies to all sessions. The longest matching prefix wins; a session-scoped
mapping wins over an unscoped mapping at the same prefix length. Equally specific
matches are ambiguous and cannot be read. Use distinct prefixes or tags to
disambiguate. Two sessions can map `/build/application/main.rs` to different roots.

All authenticated API readers can browse configured exports. Tags describe
source ownership and mapping, not per-user access control. Export only directories
you intend those readers to access. Parent traversal and symlink escapes are
rejected. A symlink is visible as a link entry; resolving its root-relative path
can read it only if the target stays within the granted directory.

## Operations

The canonical `ddb.api.v2.DebuggerService` operations are available through the
HTTP/ProtoJSON and gRPC transports, with `READ` permission:

| Operation | Purpose |
| --- | --- |
| `ListSources` | Discover debugger-known files, optionally filtered by target and path substring. |
| `ListSourceRoots` | List explicitly exported roots, including when no sessions exist. |
| `ListSourceEntries` | List one root-relative directory, including files absent from debug information. |
| `ResolveSource` | Resolve an exact debugger path with an owner, an opaque source reference, or a root reference plus relative path. |
| `ReadSource` | Read a bounded, one-based line window using a source reference. |

The Rust SDK has typed methods for all five operations. `collect_sources` follows
pages with a caller-supplied bound and retains discovery warnings. It requests
refresh only on the first page. `collect_source_roots` and `collect_source_entries`
also bound the collected result. The generated TypeScript and Python contracts
contain the same operations and resource types.

Source references are server-instance-local opaque identities. Debugger sources
include their owning session, even when sessions share a group or path. Workspace
sources include their root and relative path. Do not compare filenames, suffixes,
or equal path strings to infer document identity. A path shared by multiple
sessions requires an explicit owner or source reference when resolving it.

`Session.tag` is the configured session tag. Use it to associate explicit
frontend-local source mappings with `SourceOwner.session_id` through the current
session projection. Do not parse `display_name` or assume tags are opaque API
resource IDs. A session recreated after restart can have the same tag and a new
ID. Pair remote source/root references with `ResponseContext.server_instance_id`
and reject mismatched response contexts before using their contents.

`SourceFile.availability` is a discovery-time hint, not a promise that a later
read will succeed. Missing debugger files remain in the catalog with a reason.
Reads recheck current filesystem access; directories, binary/non-UTF-8 files,
oversized files, and escaped links cannot return source text. Workspace entries
carry an optional `debugger_path` for an explicitly configured mapping. A frontend
must not send a DDB-host display path as a remote breakpoint location.

## Refresh and pagination

`ListSources` never interrupts running targets. It uses cached catalogs for those
sessions and returns warnings that the result may be incomplete. Stopped sessions
can populate the catalog; `refresh: true` replaces their cached listings. Refresh
is invalid with a continuation token. Per-session discovery failures retain the
cached listing and produce warnings, rather than silently claiming completeness.
Catalogs have stable session/path ordering and revision-bound page tokens.

The first page of `ListSourceEntries` always enumerates that directory afresh.
Continuation pages may reuse the snapshot for at most 30 seconds. DDB reopens the
directory and checks enumeration permissions before reuse. Observed metadata
changes or a fresh first-page request invalidate a cached snapshot. Changed
entry sets expire old tokens. Filesystem timestamps are only invalidation hints;
use a new first-page request when the user explicitly refreshes. Listing is never
recursive. Unsupported filenames are omitted with an explicit count.

`ReadSource` returns `total_lines` as well as `line_count`, `has_more`, and a content
hash. An empty file has zero lines. A final newline terminates the final line;
it does not create another line. Requests beyond EOF return an empty window and
the actual total. Frontends can clamp the cursor and request the final window.
Blank lines remain distinguishable from an empty window through `line_count`.

## Resource bounds

The source workspace implementation currently enforces:

- At most 32 configured roots and 128 private directory handles for exact
  debugger-known files outside exported roots.
- Four concurrent blocking filesystem operations.
- A 2 MiB file limit and the API's advertised source-window line limit.
- At most 100,000 entries and 16 MiB of entry data per directory enumeration.
- At most 100,000 catalog results and 16 MiB of identity data per catalog request.
- A global cached debugger catalog bound of 100,000 positive/negative records,
  16 MiB of path/record data, and 4,096 sessions. Failed updates retain prior data.
- Shared 32 MiB/32-entry content and directory-snapshot caches, independently
  bounded across all roots. Pinned in-flight results remain valid after eviction.

Repeated line windows reuse the immutable file text and line index after opening
the authorized path and checking its version. Repeated directory pages clone only
the requested window from a cached snapshot. Exceeding a bound returns a typed
error instead of a silently truncated success. Narrow the catalog target/query
or export a smaller workspace when necessary.
