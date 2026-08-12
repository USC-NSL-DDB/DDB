# Builds the application under test (faketime_pause).
#
# It links the DDB connector, which reports the process over MQTT so DDB can
# discover it and attach gdb. The connector is header-only but its service
# reporter uses the paho MQTT C client, hence -lpaho-mqtt3c.
#
# The connector headers are taken straight out of the repo (../../connector),
# so this never depends on `make -C connector install` having been run.

CXX ?= g++
CXXFLAGS ?= -std=c++17 -Wall -Wextra -O2 -g
LDFLAGS ?= -lpaho-mqtt3c -lpthread

REPO_ROOT := $(abspath $(CURDIR)/../..)
CONNECTOR_INC := $(REPO_ROOT)/connector/include

BUILD_DIR := build
SRC_DIR := src
INC_DIR := include

BIN := $(BUILD_DIR)/faketime_pause
OBJS := $(BUILD_DIR)/faketime_pause.o \
        $(BUILD_DIR)/real_timer.o \
        $(BUILD_DIR)/cycle_counter.o

INCLUDES := -I$(INC_DIR) -I$(CONNECTOR_INC)

.PHONY: all clean

all: $(BIN)

$(BIN): $(OBJS)
	$(CXX) $(CXXFLAGS) -o $@ $^ $(LDFLAGS)

$(BUILD_DIR)/%.o: $(SRC_DIR)/%.cpp | $(BUILD_DIR)
	$(CXX) $(CXXFLAGS) $(INCLUDES) -c $< -o $@

$(BUILD_DIR):
	mkdir -p $(BUILD_DIR)

clean:
	rm -rf $(BUILD_DIR)
