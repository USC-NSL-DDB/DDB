# Historical configuration examples

These nine `.yaml.txt` files preserve the old `Components` configuration format.
The current DDB loader does not implement that field and previously ignored it.
Strict configuration parsing now rejects it. These files are not runnable
examples and are deliberately excluded from the active YAML example catalog.

Use `StaticSessions` for supported local launch and PID attachment, or
`ServiceDiscovery` with instrumented applications for automatic attachment.
Remote binary launch described by some historical examples is not implemented;
do not translate it into a configuration that silently changes its meaning.

See the [current debugger guide](../../../../ddb-tui/README.md#one-project-configuration)
and [helloworld example](../../../../ddb-tui/examples/helloworld.yaml).
