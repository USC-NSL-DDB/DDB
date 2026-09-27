# GDB filtered variable inspection can loop on an optimized-out argument

## Finding

Reproduced on Ubuntu GDB 15.1 (`15.1-1ubuntu1~24.04.1`) on x86-64.
The defect is in GDB's filtered MI argument printer, not DDB's filter rules,
RPC deadlines, or variable pretty printers.

In GDB's `gdb/python/py-framefilter.c`, `enumerate_args` fetches an argument
before entering its loop. It advances the iterator at the bottom of the loop.
However, when `mi_should_print` rejects a symbol, it takes a `continue` branch
before that advance. GDB repeatedly calls `symbol()` and `value()` on the same
argument forever. The rejection includes `LOC_OPTIMIZED_OUT` and `LOC_CONST`.

Primary source:
[GDB 15.1 py-framefilter.c, enumerate_args](https://gnu.googlesource.com/binutils-gdb/+/refs/tags/gdb-15.1-release/gdb/python/py-framefilter.c#443).
The upstream repair must advance the argument iterator even when an argument
is skipped, while preserving separator placement and Python error handling.
DDB does not vendor or patch the installed GDB executable.

## Evidence

- The original greeter server reproduction hangs in the inline
  `absl::CondVar::WaitWithTimeout` frame. Its `mu` argument has
  `gdb.SYMBOL_LOC_OPTIMIZED_OUT` address class 13.
- Replacing DDB's filter with an identity filter reproduces the hang.
  DDB's matching rules and presets are therefore unnecessary to trigger it.
- `--no-values` also hangs. Reading or formatting a variable's value is not
  required to trigger it.
- A temporary counter in `SymValueWrapper.symbol` showed 20 successive calls
  on the same `mu` wrapper. Raising an exception at that point exits the loop.
  The counter was only installed in an isolated diagnostic GDB process.
- The C++ fixture below reproduces the same optimized-out argument without
  DDB, gRPC, threads, or a running RPC. Its filtered command times out after
  three seconds; the unfiltered command returns the local `visible = 8`.
- The original greeter reproduction with `--no-frame-filters` completed 299
  frame-variable reads across 38 threads.

## Minimal reproduction

From the repository root, compile the checked-in fixture with GCC:

```sh
g++ -g -O2 ddb/core/tests/fixtures/optimized_frame_args.cc -o /tmp/ddb-filter-example
```

Run these commands in a disposable GDB process attached only to that fixture:

```text
gdb -nx -q /tmp/ddb-filter-example
(gdb) break stop_here
(gdb) run
(gdb) python gdb.frame_filters['identity'] = type('Identity', (), {'enabled': True, 'priority': 100, 'filter': lambda self, frames: frames})()
(gdb) interpreter-exec mi "-enable-frame-filters"
(gdb) interpreter-exec mi "-stack-list-variables --thread 1 --frame 1 --all-values"
```

On the affected GDB, the last command loops. Run the reproduction under an
external process timeout; the loop may not service debugger interrupts.
In a fresh process, replace that final command with:

```text
(gdb) interpreter-exec mi "-stack-list-variables --thread 1 --frame 1 --no-frame-filters --all-values"
```

It returns promptly with `visible = 8`. No production debugger processes or
system GDB Python files need to be changed to run this comparison.

## DDB's responsibility

The canonical variable API takes an opaque handle identifying a physical
frame. Inspecting that frame should not depend on presentation filters, which
can omit, reorder, or decorate frames and their arguments. Keep
`--no-frame-filters` in `DdbApplicationService::list_variables` for that API.
Value pretty printers still apply, and stack presentation still uses filters.
This is a deliberate API boundary and mitigation for the GDB defect, not a
claim that the installed GDB has been repaired.

The real-backend regression
`gdb_inspects_optimized_inline_frames_with_filters_enabled` verifies that the
fixture actually contains an optimized-out inline argument, enables an
identity frame filter, and reads `visible = 8` through DDB's typed API.
The earlier throwing-decorator test also checks independence from arbitrary
presentation filters.

```sh
cargo test --manifest-path ddb/Cargo.toml -p ddb --test api_v2_real_backends gdb_
```

Explicit raw MI commands that opt into GDB's filtered argument printer can
still trigger the upstream bug. No adapter workaround or system-wide Python
monkey patch was added. A complete repair of that raw GDB behavior belongs
in GDB's argument-iteration implementation.
