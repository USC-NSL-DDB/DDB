# Init flags and common variables
include ./build/config.mk

FLAGS	= -g -Wall
LDFLAGS = -T 
GCC     ?= gcc-13
GXX     ?= g++-13
LD      = $(GCC)
CC      = $(GCC)
LDXX	= $(GXX)
CXX		= $(GXX)
AR      = ar

ifeq ($(CONFIG_DEBUG),y)
FLAGS += -DDEBUG -O0 -ggdb
# LDFLAGS += -rdynamic
else
FLAGS += -DNDEBUG -O3
endif

CFLAGS = $(FLAGS)
CXXFLAGS = -std=gnu++23 $(FLAGS)

BIN_FOLDER := ./bin

# Build
TEST_BINARIES_PATH = test_binaries

NCORES = $(shell nproc)

.PHONY: all
all: test_binaries

# Automatically create bin folder when necessary.
$(BIN_FOLDER):
	mkdir -p $@

%.o: %.cpp
	$(CXX) $(CXXFLAGS) -c $< -o $@

test_hello_world_src = $(TEST_BINARIES_PATH)/hello_world.cpp
test_hello_world_obj = $(test_hello_world_src:.cpp=.o)

test_nested_frame_src = $(TEST_BINARIES_PATH)/nested_frame.cpp
test_nested_frame_obj = $(test_nested_frame_src:.cpp=.o)

test_multithread_print_src = $(TEST_BINARIES_PATH)/multithread_print.cpp
test_multithread_print_obj = $(test_multithread_print_src:.cpp=.o)

test_multiproc_print_src = $(TEST_BINARIES_PATH)/multiprocess.cpp
test_multiproc_print_obj = $(test_multiproc_print_src:.cpp=.o)

test_arg_pass_src = $(TEST_BINARIES_PATH)/arg_pass.cpp
test_arg_pass_obj = $(test_arg_pass_src:.cpp=.o)

bin/hello_world: $(test_hello_world_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_hello_world_obj)
bin/nested_frame: $(test_nested_frame_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_nested_frame_obj)
bin/multithread_print: $(test_multithread_print_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_multithread_print_obj) -lpthread
bin/multiprocess: $(test_multiproc_print_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_multiproc_print_obj) -lpthread
bin/arg_pass: $(test_arg_pass_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_arg_pass_obj)

test_binaries: $(BIN_FOLDER) bin/hello_world bin/nested_frame bin/multithread_print bin/multiprocess bin/arg_pass

gdb: 
	pushd gdb-14.1
	mkdir -p build
	pushd build
	../configure && make -j$(NCORES)

.PHONY: clean
clean:
	rm -rf $(TEST_BINARIES_PATH)/*.o bin/* 
