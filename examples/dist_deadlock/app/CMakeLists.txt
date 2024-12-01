cmake_minimum_required(VERSION 3.11)
project(apps CXX)

# Find all test source files
file(GLOB TEST_SOURCES "${CMAKE_CURRENT_SOURCE_DIR}/*.cpp")

# For each test source file, create a test executable and link with the librafty
foreach(test_source ${TEST_SOURCES})
    # Get the test name from the file name
    get_filename_component(test_name ${test_source} NAME_WE)

    # Create the test executable
    add_executable(${test_name} ${test_source})

    # Link the test executable with the librafty and the generated protobuf library
    target_link_libraries(${test_name} PRIVATE 
        example comm_grpc_proto comm_grpc_proto
        absl::check
        absl::flags
        absl::flags_parse
        absl::log
        ${_REFLECTION}
        ${_GRPC_GRPCPP}
        ${_PROTOBUF_LIBPROTOBUF}
        pthread
    )

endforeach()
