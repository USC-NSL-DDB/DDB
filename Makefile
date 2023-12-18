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

# test_binaries_src = $(wildcard $TEST_BINARIES_PATH/*.cpp)
# test_binaries_obj = $(test_binaries_src:.cpp=.o)

# Automatically create bin folder when necessary.
$(BIN_FOLDER):
	mkdir -p $@

%.o: %.cpp
	$(CXX) $(CXXFLAGS) -c $< -o $@

test_hello_world_src = $(TEST_BINARIES_PATH)/hello_world.cpp
test_hello_world_obj = $(test_hello_world_src:.cpp=.o)

bin/hello_world: $(test_hello_world_obj)
	$(CXX) $(CXXFLAGS) -o $@ $(test_hello_world_obj)

test_binaries: $(BIN_FOLDER) bin/hello_world

.PHONY: all
all: test_binaries

.PHONY: clean
clean:
	rm -rf $(TEST_BINARIES_PATH)/*.o bin/*
