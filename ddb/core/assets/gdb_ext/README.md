# GDB Extension Scripts

This folder contains all GDB extension scripts that are sourced by DDB at runtime to extend GDB capabilities.

## Dev

For dev, you can set up LSP (assumes Pylance). All required packages (stubs) can be installed via `uv` by running (you may need to install `uv` first):

``` bash
uv sync
```

As GDB python module is a special case, which cannot be installed via pip. The GDB module comes with your gdb installation. Therefore, we can only refer to GDB stub maintained by the community from this package: https://pypi.org/project/types-gdb

There is no GDB python source available unless you go to GDB directory to look at the C/C++ code.
