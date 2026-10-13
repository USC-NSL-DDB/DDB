cmake_minimum_required(VERSION 3.16)
project(ddb_greeter_fixture LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
get_filename_component(DDB_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../../../.." ABSOLUTE)

# Compile the pinned instrumented framework with this checkout's connector.
# No preinstalled, potentially uninstrumented gRPC is accepted.
include_directories(BEFORE "${DDB_ROOT}/connector/include")
find_package(Threads REQUIRED)
add_executable(ddb_attach_signal_order attach_signal_order.cc)
target_compile_options(ddb_attach_signal_order PRIVATE -UNDEBUG)
target_link_libraries(ddb_attach_signal_order PRIVATE Threads::Threads)
set(gRPC_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(gRPC_INSTALL OFF CACHE BOOL "" FORCE)
set(protobuf_INSTALL OFF CACHE BOOL "" FORCE)
set(gRPC_SSL_PROVIDER package CACHE STRING "" FORCE)
set(gRPC_CARES_PROVIDER package CACHE STRING "" FORCE)
set(gRPC_ZLIB_PROVIDER package CACHE STRING "" FORCE)
add_subdirectory("${DDB_ROOT}/fwks/grpc" "${CMAKE_CURRENT_BINARY_DIR}/grpc" EXCLUDE_FROM_ALL)
find_library(PAHO_MQTT_LIBRARY NAMES paho-mqtt3c REQUIRED)

set(PROTO_ROOT "${DDB_ROOT}/fwks/grpc/examples/protos")
add_custom_command(
  OUTPUT helloworld.pb.cc helloworld.pb.h helloworld.grpc.pb.cc helloworld.grpc.pb.h
  COMMAND $<TARGET_FILE:protobuf::protoc>
    --proto_path=${PROTO_ROOT}
    --cpp_out=${CMAKE_CURRENT_BINARY_DIR}
    --grpc_out=${CMAKE_CURRENT_BINARY_DIR}
    --plugin=protoc-gen-grpc=$<TARGET_FILE:grpc_cpp_plugin>
    ${PROTO_ROOT}/helloworld.proto
  DEPENDS "${PROTO_ROOT}/helloworld.proto" protobuf::protoc grpc_cpp_plugin
  VERBATIM)
add_library(greeter_proto helloworld.pb.cc helloworld.grpc.pb.cc)
target_include_directories(greeter_proto PUBLIC "${CMAKE_CURRENT_BINARY_DIR}")
target_link_libraries(greeter_proto PUBLIC grpc++ protobuf::libprotobuf "${PAHO_MQTT_LIBRARY}")

foreach(role client server)
  add_executable(greeter_${role} greeter_${role}.cc)
  target_compile_options(greeter_${role} PRIVATE -O0 -g -fno-omit-frame-pointer)
  target_link_libraries(greeter_${role} PRIVATE greeter_proto absl::flags absl::flags_parse)
endforeach()
