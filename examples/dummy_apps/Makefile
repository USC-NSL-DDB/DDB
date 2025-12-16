# Init flags and common variables
include ../../build/config.mk

SHELL=/bin/bash

FLAGS	= -g -Wall
GCC     ?= gcc
GXX     ?= g++
LD      = $(GCC)
CC      = $(GCC)
LDXX	= $(GXX)
CXX		= $(GXX)
AR      = ar

FLAGS += -DDEBUG -O0 -ggdb

CFLAGS = $(FLAGS)
CXXFLAGS = -std=gnu++23 $(FLAGS) -MMD -MP

BIN_FOLDER := ./bin

# Directory for include files (ddb)
INCLUDE_DIR = $(PREFIX)/include

DDB_HDRS_PREFIX = connector/include
DDB_CEREAL_HDRS = $(DDB_HDRS_PREFIX)/cereal
DDB_HDRS = $(DDB_HDRS_PREFIX)/ddb

# Build
TEST_BINARIES_PATH = . 

NCORES = $(shell nproc)

.DEFAULT_GOAL := all

.PHONY: all help clean test_binaries

# Help target
help:
	@echo "Available targets:"
	@echo "  all              - Build all test binaries (default)"
	@echo "  test_binaries    - Build all test binaries"
	@echo "  bin/hello_world  - Build hello_world binary"
	@echo "  bin/nested_frame - Build nested_frame binary"
	@echo "  bin/multithread_print - Build multithread_print binary"
	@echo "  bin/multiprocess - Build multiprocess binary"
	@echo "  bin/arg_pass     - Build arg_pass binary"
	@echo "  bin/go_dummy     - Build go_dummy binary"
	@echo "  clean            - Remove all built files"

all: test_binaries

$(BIN_FOLDER):
	mkdir -p $@

# Pattern rule for C++ compilation with automatic dependency generation
%.o: %.cpp
	$(CXX) $(CXXFLAGS) -c $< -o $@

# Source and object file definitions
test_hello_world_src = hello_world.cpp
test_hello_world_obj = $(test_hello_world_src:.cpp=.o)

test_nested_frame_src = nested_frame.cpp
test_nested_frame_obj = $(test_nested_frame_src:.cpp=.o)

test_multithread_print_src = multithread_print.cpp
test_multithread_print_obj = $(test_multithread_print_src:.cpp=.o)

test_multiproc_print_src = multiprocess.cpp
test_multiproc_print_obj = $(test_multiproc_print_src:.cpp=.o)

test_arg_pass_src = arg_pass.cpp
test_arg_pass_obj = $(test_arg_pass_src:.cpp=.o)

test_go_dummy_src = go_dummy.go

# Binary targets with order-only dependency on bin folder
bin/hello_world: $(test_hello_world_obj) | $(BIN_FOLDER)
	$(CXX) $(CXXFLAGS) -o $@ $(test_hello_world_obj)

bin/nested_frame: $(test_nested_frame_obj) | $(BIN_FOLDER)
	$(CXX) $(CXXFLAGS) -o $@ $(test_nested_frame_obj)

bin/multithread_print: $(test_multithread_print_obj) | $(BIN_FOLDER)
	$(CXX) $(CXXFLAGS) -o $@ $(test_multithread_print_obj) -lpthread

bin/multiprocess: $(test_multiproc_print_obj) | $(BIN_FOLDER)
	$(CXX) $(CXXFLAGS) -o $@ $(test_multiproc_print_obj) -lpthread

bin/arg_pass: $(test_arg_pass_obj) | $(BIN_FOLDER)
	$(CXX) $(CXXFLAGS) -o $@ $(test_arg_pass_obj)

bin/go_dummy: $(test_go_dummy_src) | $(BIN_FOLDER)
	go build -gcflags "all=-N -l" -o $@ $(test_go_dummy_src)

test_binaries: \
	bin/hello_world \
	bin/nested_frame \
	bin/multithread_print \
	bin/multiprocess \
	bin/arg_pass \
	bin/go_dummy

clean:
	rm -rf *.o *.d bin/

# Include automatically generated dependency files
-include $(wildcard *.d)
