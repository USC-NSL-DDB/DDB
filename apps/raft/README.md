# Raft Lab Solution

## Prerequisite

- `cmake`: version 3.22.1
- `g++`: version (Ubuntu 13.1.0-8ubuntu1~22.04) 13.1.0

The version is not a hard requirement, but your lab will be graded on a machine (Ubuntu 22.04.4 LTS) with the above toolchain.

> [!IMPORTANT]
> The lab will be built with **C++20** standard and extension disabled. (`-std=c++20` is used). Please avoid incompatible APIs. For more details, you can refer to the cmake files in the project.
>
> Please post online if you have any concerns or questions.

> [!WARNING]
> You are not supposed to modify any build files. You can do that for your own testing purposes, but the grader will use the unmodified build files. Make sure your code compile correctly with provided build files.

## Build the lab

### Dependency Installation

The lab depends on a few external libraries, primarily `grpc`, `googletest`, and `spdlog`. The setup script is provided for your convenience. The setup script will clone dependencies as git submodules and they will live inside this project folder. However, `grpc`-related binaries/header files will be installed to `$HOME/.local`. Use the script with your own caution.

``` bash
# cd to the root directory
./setup.sh
```

### Configure

``` bash
mkdir build
cd build
cmake ..
```

### Build

``` bash
make -j$(nproc)
```

