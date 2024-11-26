# Init flags and common variables
include ./build/config.mk

SHELL=/bin/bash

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

# Directory for include files (ddb)
INCLUDE_DIR = $(INSTALL_PREFIX)/include

DDB_HDRS_PREFIX = connector
DDB_CEREAL_HDRS = $(DDB_HDRS_PREFIX)/cereal
DDB_HDRS = $(DDB_HDRS_PREFIX)/ddb

# Build
TEST_BINARIES_PATH = test_binaries

NCORES = $(shell nproc)

.PHONY: all
all: test_binaries nu_binaries

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

test_go_dummy_src = $(TEST_BINARIES_PATH)/go_dummy.go

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
bin/go_dummy: $(test_go_dummy_src)
	go build -o $(BIN_FOLDER)/go_dummy $(test_go_dummy_src)

test_binaries: $(BIN_FOLDER) \
	bin/hello_world bin/nested_frame bin/multithread_print bin/multiprocess bin/arg_pass \
	bin/go_dummy

clean_nu_binaries:
	rm -rf ./nu_bin ./caladan_bin

nu_binaries: 
	./compile_nu_bin.sh ./Nu all

# gdb preparation/installation for included gdb-14.2
gdb-clean:
	(cd gdb-14.2/build && make clean) > /dev/null 2>&1 || true
	rm -rf gdb-14.2/build > /dev/null 2>&1 || true

gdb: gdb-clean
	pushd gdb-14.2 && \
	mkdir -p build && pushd build && \
	../configure --disable-install-man --with-python=/usr/bin/python3 && make -j$(NCORES) && \
	popd && popd

gdb-install: gdb
	pushd gdb-14.2/build && sudo make install

install-hdrs:
	install -d $(INCLUDE_DIR)
	cp -r $(DDB_CEREAL_HDRS) $(INCLUDE_DIR)
	cp -r $(DDB_HDRS) $(INCLUDE_DIR)

.PHONY: rpc-framework-setup
rpc-framework-setup:
	cd ./scripts && ./rpc_framework_setup.sh

.PHONY: gdb-config-setup
gdb-config-setup:
	cd ./scripts && ./setup_gdb.sh

.PHONY: clean
clean: clean_nu_binaries gdb-clean
	rm -rf $(TEST_BINARIES_PATH)/*.o bin/*
