cmake_minimum_required(VERSION 3.11)
project(raft_unittests_internals CXX)

file(GLOB TEST_SOURCES "${CMAKE_CURRENT_SOURCE_DIR}/*.cpp")

foreach(test_source ${TEST_SOURCES})
    get_filename_component(test_name ${test_source} NAME_WE)

    add_executable(${test_name} ${test_source} ${absl_INCLUDE_DIRS} ${Protobuf_INCLUDE_DIRS} )

    target_link_libraries(${test_name} PRIVATE 
        rafty raft_grpc_proto gtest gtest_main
    )
endforeach()