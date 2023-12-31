## Build on `aarch64-apple-darwin`

Building gdb requires GMP 4.2+, and MPFR 3.1.0+. I got no success to build on M-series chips yet. Looks like there are some ported pre-build gdb out there, but not sure how they approach this.

## Machine Interface

Though the `arm64` may not be an important platform to support for the current stage, but LLDB can be helpful on these platforms due to Apple's first-party support. Supporting LLDB is useful as it offers more flexibility to other languages (beyond gdb). LLDB also has similar machine interface (MI), just as GDB.
