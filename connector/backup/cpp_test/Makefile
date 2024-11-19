CC = gcc
CXX = g++
CFLAGS = -I/usr/local/include
CXXFLAGS = -I/usr/local/include -I../include
LDFLAGS = -L/usr/local/lib -lpaho-mqtt3c

test: main.cpp
	$(CXX) -g $(CXXFLAGS) -o test main.cpp $(LDFLAGS)
