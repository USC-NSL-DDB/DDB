# Instrumented gRPC greeter fixture

These client/server programs adapt the Apache-licensed gRPC helloworld example
for reproducible DDB automatic-attach terminal tests. The framework is the exact
`fwks/grpc` gitlink committed in this repository. Connector headers come from
this checkout. System gRPC packages are deliberately not used.

Both applications accept `--ddb`, `--ddb_host_ip`, and `--ddb_proc_alias`. With
`--ddb`, they report through the real MQTT connector and wait for DDB to attach.
On Linux they explicitly permit same-user debugger attachment instead of
requiring the test runner to disable its host-wide Yama policy. No debugger
events or application responses are simulated.

Build from the repository root using `bash ddb/tools/build-ci-greeters.sh` after
initializing the gRPC, Abseil, Protobuf, and RE2 submodules. The build requires
CMake, Ninja, ccache, a C++17 compiler, OpenSSL, c-ares, zlib, and Paho MQTT C
development files. The output is `build/greeter_client` and
`build/greeter_server`. Debug information and disabled optimization are required
for source breakpoints, stepping, and variable inspection.

Set `DDB_GREETER_WORKSPACE` to this directory when running the TUI terminal
suite. Runtime prerequisites are GDB, Mosquitto, `ddb_runapp`, libfaketime, and
GCC's libstdc++ Python printers at `/usr/share/gcc/python`. Tests own their
brokers and processes and preserve the prior discovery configuration.
