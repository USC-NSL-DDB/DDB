# Simple integration test suite for libfaketime interposition behavior.

CC ?= gcc

CFLAGS += -std=gnu99 -Wall -Wextra -O0 -g
LDFLAGS += -ldl -pthread

BINDIR := bin
BIN := $(BINDIR)/faketime_immature_wakeup

.PHONY: all clean test build_libfaketime

all: build_libfaketime $(BIN)

build_libfaketime:
	$(MAKE) -C ../../libfaketime/src all

$(BINDIR):
	mkdir -p $(BINDIR)

$(BIN): faketime_immature_wakeup.c | $(BINDIR)
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

test: all
	bash ./run.sh

clean:
	rm -rf $(BINDIR)
