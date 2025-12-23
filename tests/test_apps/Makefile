# Makefile for read_time.cpp

CXX = g++
CXXFLAGS = -std=c++20 -Wall -O0 -g
BINDIR = bin
TARGET = $(BINDIR)/read_time
SRC = read_time.cpp

all: $(TARGET)

$(TARGET): $(SRC) | $(BINDIR)
	$(CXX) $(CXXFLAGS) -o $(TARGET) $(SRC)

$(BINDIR):
	mkdir -p $(BINDIR)

clean:
	rm -rf $(BINDIR)
