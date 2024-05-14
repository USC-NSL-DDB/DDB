CC = gcc
CXX = g++
CFLAGS = -I/usr/local/include
CXXFLAGS = -I/usr/local/include
LDFLAGS = -L/usr/local/lib -lpaho-mqtt3c

all: mqtt_publisher

mqtt_publisher: mqtt_publisher.o
	$(CXX) -o test_binaries/mqtt_publisher mqtt_publisher.o $(LDFLAGS)

mqtt_publisher.o: test_binaries/mqtt_publisher.cpp
	$(CXX) $(CFLAGS) -c test_binaries/mqtt_publisher.cpp

clean:
	rm -f mqtt_publisher mqtt_publisher.o
