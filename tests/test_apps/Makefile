# Makefile for test applications

CXX = g++
INCLUDE_DIRS = -I../../connector/include/ -Iinc
LDFLAGS = -L$(HOME)/.local/lib -lpaho-mqtt3c -lpthread
CXXFLAGS = -std=c++20 -Wall -O0 -g
BINDIR = bin

# Find all .cpp files in the current directory
SOURCES = $(wildcard *.cpp)
# Generate target names: strip .cpp and add bin/ prefix
TARGETS = $(patsubst %.cpp,$(BINDIR)/%,$(SOURCES))

all: $(TARGETS)

# Pattern rule to compile each .cpp file into its own executable
$(BINDIR)/%: %.cpp | $(BINDIR)
	$(CXX) $(CXXFLAGS) $(INCLUDE_DIRS) -o $@ $< $(LDFLAGS)

$(BINDIR):
	mkdir -p $(BINDIR)

clean:
	rm -rf $(BINDIR)

.PHONY: all clean
