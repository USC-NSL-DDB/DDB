CC = gcc
CXX = g++
CFLAGS = -I/usr/local/include -Iinclude
CXXFLAGS = -I/usr/local/include -Iinclude
LDFLAGS = -L/usr/local/lib -lpaho-mqtt3c

all: mqtt_publisher test_reporter

prep_header:
	@rm -rf include
	@mkdir -p include
	@cp -r ../../include ./

mqtt_publisher: mqtt_publisher.o
	$(CXX) -o test_binaries/mqtt_publisher mqtt_publisher.o $(LDFLAGS)

mqtt_publisher.o: test_binaries/mqtt_publisher.cpp
	$(CXX) $(CXXFLAGS) -c test_binaries/mqtt_publisher.cpp

test_reporter: test_reporter.o
	$(CXX) -o test_reporter test_reporter.o $(LDFLAGS)

test_reporter.o: prep_header test_reporter.cpp
	$(CXX) $(CXXFLAGS) -c test_reporter.cpp

run_broker:
	@sudo mosquitto -c ../../../conf/mosquitto.conf > /dev/null 2>&1 &

stop_broker:
	@sudo pkill -9 mosquitto

clean:
	rm -f *.o test_binaries/mqtt_publisher test_reporter
