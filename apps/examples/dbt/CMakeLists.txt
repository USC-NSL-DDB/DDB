cmake_minimum_required(VERSION 3.10)
project(Rafty)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED True)
set(CMAKE_CXX_EXTENSIONS OFF)

set(CMAKE_BUILD_TYPE Debug CACHE STRING "Build type (default is Debug)")

if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    add_compile_definitions(DDB_DEBUG)
endif()

set(CMAKE_CXX_FLAGS "${CMAKE_CXX_FLAGS} -std=c++20 -Wall -Wextra")
set(CMAKE_CXX_FLAGS_DEBUG "-g")
set(CMAKE_CXX_FLAGS_RELEASE "-O3 -g")

# This depends on how the setup script install the grpc
set(grpc_prefix "$ENV{HOME}/.local")

set(CMAKE_PREFIX_PATH ${grpc_prefix} CACHE PATH "libs prefix path")

# includes grpc
include(cmake/common.cmake)

# Add the include directories to the project
# include_directories(${CMAKE_SOURCE_DIR}/inc)
include_directories(${CMAKE_SOURCE_DIR}/generated)

# Add subdirectories
add_subdirectory(proto)
add_subdirectory(src)
