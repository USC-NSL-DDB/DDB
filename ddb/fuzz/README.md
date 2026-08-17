# DDB public API fuzz targets

These standalone targets exercise untrusted canonical binary Protobuf and
ProtoJSON envelopes without adding fuzz-only dependencies to production crates.
They decode and re-encode representative recursive, event, operation, extension,
error, cursor, and snapshot messages.

Prerequisites are a nightly Rust toolchain, a C++ compiler, and `cargo-fuzz`
0.13.2. From `ddb/`:

```bash
cargo +nightly fuzz run --fuzz-dir fuzz protobuf_contract -- \
  -max_len=1048576 -max_total_time=60
cargo +nightly fuzz run --fuzz-dir fuzz protojson_contract -- \
  -max_len=1048576 -max_total_time=60
```

The one-MiB input bound prevents a corpus artifact from bypassing the intended
test resource envelope. Production HTTP/gRPC request limits remain independent
and are tested at the process boundary. Commit a minimized regression input
only when it contains no source, memory, output, expression, command, or
credential data.

Dated local smoke evidence is retained under [`evidence/`](evidence/).
