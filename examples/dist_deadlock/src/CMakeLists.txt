file(GLOB SOURCE_FILES "${CMAKE_SOURCE_DIR}/src/*.cpp")

message(DEBUG "SOURCE_FILES: ${SOURCE_FILES}")

file(GLOB_RECURSE HEADER_FILES "${CMAKE_SOURCE_DIR}/inc/**/*.hpp")

add_library(example STATIC
    ${SOURCE_FILES} 
    ${HEADER_FILES}
)

# Link the library with the spdlog, absl, grpc, protobuf, and other libraries
target_link_libraries(
    example comm_grpc_proto 
    absl::check
    absl::flags
    absl::flags_parse
    absl::log
    ${_REFLECTION}
    ${_GRPC_GRPCPP}
    ${_PROTOBUF_LIBPROTOBUF}
)
