file(GLOB SOURCE_FILES "${CMAKE_SOURCE_DIR}/src/*.cpp")

message(DEBUG "SOURCE_FILES: ${SOURCE_FILES}")

file(GLOB_RECURSE HEADER_FILES "${CMAKE_SOURCE_DIR}/inc/**/*.hpp")

add_library(rafty STATIC
    ${SOURCE_FILES} 
    ${HEADER_FILES}
)

# Link the library with the spdlog, absl, grpc, protobuf, and other libraries
target_link_libraries(
    rafty raft_grpc_proto 
    spdlog::spdlog $<$<BOOL:${MINGW}>:ws2_32>
    absl::check
    absl::flags
    absl::flags_parse
    absl::log
    ${_REFLECTION}
    ${_GRPC_GRPCPP}
    ${_PROTOBUF_LIBPROTOBUF}
)

# Check the compiler and add the appropriate flag
# if(CMAKE_CXX_COMPILER_ID STREQUAL "GNU" OR CMAKE_CXX_COMPILER_ID STREQUAL "Clang")
#     target_compile_options(rafty PRIVATE -Werror=deprecated-declarations)
# elseif(CMAKE_CXX_COMPILER_ID STREQUAL "MSVC")
#     target_compile_options(rafty PRIVATE /we4996)
# endif()