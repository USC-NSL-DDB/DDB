# API v2 release-readiness evidence: 2026-08-14

This record captures the verification performed against the API v2 implementation
before it is split into reviewable commits. It is evidence for the current
working tree, not a claim that uncommitted files have been released.

## Revision and environment

- Baseline repository commit: `845846460fac30aa8614430f82753e86ab664b71`
- Working tree: intentionally dirty; the API platform and `ddb-tui` implementation
  have not been committed by this verification run.
- Rust: `rustc 1.89.0 (29483883e 2025-08-04)`, LLVM 20.1.7,
  `x86_64-unknown-linux-gnu`
- GDB: 14.2
- LLDB: 18.1.3
- Buf: 1.72.0, matching CI
- Benchmark binary and environment hashes are recorded in the three evidence
  directories linked below.

No credentials, debugger source contents, memory contents, or raw command text
are retained in this record.

## Correctness and package gates

All commands below completed successfully unless a qualification is stated.

Backend workspace, from `ddb/`:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy -p ddb --all-targets --all-features --no-deps -- -D warnings
cargo clippy -p ddb-bench --all-targets --no-deps -- -D warnings
cargo test --workspace --all-targets
cargo test -p ddb --all-targets --all-features
cargo test -p ddb --release thread_status_mutation_is_not_debug_only
```

The release-only test protects the runtime mutation that previously lived
inside `debug_assert!` and therefore disappeared in optimized builds.

Reference frontend, from `ddb-tui/`:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
cargo test --test e2e_mock -- --ignored --nocapture
```

The ignored PTY run passed all five workflows: Mock, reconnect/start-late,
explicit v1 fallback, real GDB, and real LLDB. The Mock and real-debugger cases
exercise the independent execution marker (`▶`), navigation cursor (`▸`), and
breakpoint marker (`●`).

Lifecycle and public SDKs:

```bash
cargo test -p ddb --test api_v2_language_sdks -- --ignored --nocapture
cargo test -p ddb --test api_v2_soak -- --ignored --nocapture
npm ci --ignore-scripts
npm run check
npm test
npm pack --dry-run
env PYTHONPATH=src python3 -m compileall -q src tests examples
env PYTHONPATH=src python3 -m unittest discover -s tests -v
env SOURCE_DATE_EPOCH=1704067200 python3 -m pip wheel . --no-deps --wheel-dir <temporary-directory>
./tools/check-api-release.sh
```

The language SDK process test used a real Mock-backed DDB process. The soak
covered journal rollover, rehydration, reconnect, and a slow output consumer
without blocking the control lane. The release dry run reproduced and packaged
the public Rust crates, TypeScript package, and Python wheel without publishing.

## Contract and generated artifacts

```bash
npx --yes @bufbuild/buf@1.72.0 format --diff --exit-code
npx --yes @bufbuild/buf@1.72.0 lint
cargo run -p ddb-api-codegen -- --check
cargo test -p ddb-api-types --all-targets
npx --yes @redocly/cli@2.46.1 lint docs/api/generated/openapi-v2.json
npx --yes @asyncapi/cli@6.0.2 validate docs/api/generated/asyncapi-v2.json --diagnostics-format json
```

Generation reproduced Rust, TypeScript, Python, the descriptor set, OpenAPI,
and AsyncAPI exactly. AsyncAPI reported no governance issues. Redocly accepted
the OpenAPI document with four `no-unused-components` warnings for schema types
that remain part of the canonical Protobuf contract but are not referenced by
an HTTP operation.

`buf breaking` was not run against a released v2 descriptor because this is the
initial v2 schema and the baseline commit contains no `ddb/proto` tree. CI is
configured to establish that first baseline and to enforce `FILE` compatibility
on every later pull request whose base contains the schema.

## Fuzz and untrusted-input evidence

Temporary, empty corpora were used so fuzz discovery did not modify the
checked-in seeds:

```bash
cargo +nightly fuzz run protobuf_contract <temporary-corpus> -- \
  -max_total_time=15 -rss_limit_mb=2048 -print_final_stats=1
cargo +nightly fuzz run protojson_contract <temporary-corpus> -- \
  -max_total_time=15 -rss_limit_mb=2048 -print_final_stats=1
```

- Protobuf: 352,405 executions in 16 seconds; no crash or timeout; 528 MB peak RSS.
- ProtoJSON: 299,467 executions in 16 seconds; no crash or timeout; 517 MB peak RSS.

The ordinary workspace and all-feature suites additionally cover bounded HTTP
bodies and encoded responses, malformed requests and streams, auth scopes,
CORS, remote binding, rate/concurrency limits, source policy, replay gaps,
operation/idempotency limits, slow subscribers, graceful drain, and real
GDB/LLDB projection.

## Performance evidence

- [HTTP/gRPC snapshot comparison](../../benchmarks/evidence/2026-08-14-v2-transport/README.md)
- [Typed control and slow-output comparison](../../benchmarks/evidence/2026-08-14-v2-control-output/README.md)
- [Large inspection, memory, fanout, and reconnect/replay](../../benchmarks/evidence/2026-08-14-v2-inspection-replay/README.md)

Each comparison retains three raw JSON runs, an environment manifest, exact
commands, and binary hashes. The typed control evidence directly caught two
production defects during refinement: optimized builds compiling out a thread
state mutation, and a slow output subscriber contending with other consumers.
Both were fixed and protected by regression tests.

The frontend-workload matrix additionally validates exact 10,000-variable
paging, advancing bounded memory chunks at 1/16/64 MiB, agreement across
1/8/20 state subscribers, and forced reconnect/replay convergence at 1/16/64
Mock sessions. Non-divisible variable and memory chunk smoke cases were run
during refinement to check exact accounting rather than only the retained
round-number matrix.

The retained evidence does not measure allocator events, CPU time, complete
wire bytes, or real-backend performance. Consequently, gRPC remains an optional
preview and no broad resource-efficiency claim is made. This is consistent with
[ADR 0005](adr/0005-transport-policy.md); promotion requires the broader matrix
defined there.

## Release actions not performed

The following are maintainer/release actions, not missing runtime behavior:

- split and commit the dirty working tree according to the logical commit plan;
- run `buf breaking` against the first published v2 descriptor on later changes;
- publish or sign packages under a project release policy; and
- announce general availability and begin the documented v1 support window.

The plan's out-of-process DAP adapter is also intentionally not part of this v2
preview candidate. Stage 9 places it after v2 conformance is stable; implementing
an IDE adapter before the public contract is reviewed and released would add a
second frontend project without strengthening the API boundary.
