# Faketime integration tests

These tests exercise libfaketime’s interposed timeout APIs under a mid-wait negative
step of the faketime offset (via `FAKETIME_TIMESTAMP_FILE`), verifying the wrappers
do not time out “too early” in fake time.

Run:

```sh
make -C tests/faketime_integration test
```

