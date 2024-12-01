# Proto file
get_filename_component(comm_proto "./comm.proto" ABSOLUTE)
get_filename_component(comm_proto_path "${comm_proto}" PATH)

set(comm_proto_srcs "${CMAKE_CURRENT_BINARY_DIR}/comm.pb.cc")
set(comm_proto_hdrs "${CMAKE_CURRENT_BINARY_DIR}/comm.pb.h")
set(comm_grpc_srcs "${CMAKE_CURRENT_BINARY_DIR}/comm.grpc.pb.cc")
set(comm_grpc_hdrs "${CMAKE_CURRENT_BINARY_DIR}/comm.grpc.pb.h")

add_custom_command(
      OUTPUT "${comm_proto_srcs}" "${comm_proto_hdrs}" "${comm_grpc_srcs}" "${comm_grpc_hdrs}"
      COMMAND ${_PROTOBUF_PROTOC}
      ARGS --grpc_out "${CMAKE_CURRENT_BINARY_DIR}"
        --cpp_out "${CMAKE_CURRENT_BINARY_DIR}"
        -I "${comm_proto_path}"
        --plugin=protoc-gen-grpc="${_GRPC_CPP_PLUGIN_EXECUTABLE}"
        "${comm_proto}"
      DEPENDS "${comm_proto}")

# Include generated *.pb.h files
include_directories("${CMAKE_CURRENT_BINARY_DIR}")

# Build proto files a separated static library
add_library(comm_grpc_proto STATIC
  ${comm_grpc_srcs}
  ${comm_grpc_hdrs}
  ${comm_proto_srcs}
  ${comm_proto_hdrs})

target_link_libraries(comm_grpc_proto
  absl::check
  ${_REFLECTION}
  ${_GRPC_GRPCPP}
  ${_PROTOBUF_LIBPROTOBUF}
  paho-mqtt3c
)

add_custom_target(generate_proto_files ALL DEPENDS 
  ${comm_proto_srcs}
  ${comm_proto_hdrs}
  ${comm_grpc_srcs}
  ${comm_grpc_hdrs}
)

# Create the generated directory if it doesn't exist
add_custom_command(
      TARGET comm_grpc_proto POST_BUILD
      COMMAND ${CMAKE_COMMAND} -E make_directory "${CMAKE_SOURCE_DIR}/generated"
)

# Just for convenience...
# Custom command to create symbolic links for each generated file
add_custom_command(
      TARGET comm_grpc_proto POST_BUILD
      COMMAND ${CMAKE_COMMAND} -E echo "Creating symbolic links for generated protobuf files"
      COMMAND ${CMAKE_COMMAND} -P "${CMAKE_CURRENT_BINARY_DIR}/create_symlinks.cmake"
)

# CMake script to handle the symlink creation
file(WRITE "${CMAKE_CURRENT_BINARY_DIR}/create_symlinks.cmake"
"file(GLOB proto_files \"${CMAKE_CURRENT_BINARY_DIR}/*.pb.cc\" \"${CMAKE_CURRENT_BINARY_DIR}/*.pb.h\")
foreach(proto_file \${proto_files})
    get_filename_component(filename \${proto_file} NAME)
    execute_process(COMMAND ${CMAKE_COMMAND} -E create_symlink \${proto_file} \"${CMAKE_SOURCE_DIR}/generated/\${filename}\")
endforeach()
")
