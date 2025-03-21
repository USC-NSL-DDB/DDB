CXX = g++
CXXFLAGS = -Wall -g

INCLUDE_DIRS = -I$(HOME)/.local/include
CXXFLAGS += $(INCLUDE_DIRS)

LIBFAKETIME_DIR = ../../../libfaketime
LIBFAKETIME_FLAGS = FAKETIME_NO_CACHE=1 FAKETIME="-00000000000000000"

# MQTT library linking
LDFLAGS = -L$(HOME)/.local/lib -lpaho-mqtt3c -lpthread

# Update compile rule to use LDFLAGS
main: main.cpp
	$(CXX) $(CXXFLAGS) -o main main.cpp $(LDFLAGS)
	
.PHONY: run
run: main
	./main	
	
.PHONY: run-with-ddb
run-with-ddb: main
	./main --enable_ddb --ddb_addr 127.0.0.1
	
.PHONY: compile-libfaketime
compile-libfaketime:
	$(MAKE) -C $(LIBFAKETIME_DIR)

.PHONY: run-faketime
run-faketime: main compile-libfaketime
	LD_PRELOAD=$(LIBFAKETIME_DIR)/src/libfaketime.so.1 $(LIBFAKETIME_FLAGS) ./main --enable_ddb --ddb_addr 127.0.0.1

.PHONY: clean
clean:
	rm -f main *.o
