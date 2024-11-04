# ROOT_PATH=..
# CXX = $(ROOT_PATH)/build/bin/clang++
# CC = $(ROOT_PATH)/build/bin/clang
GCC = /usr/bin/gcc
AR = /usr/bin/ar

CFLAGS = -g -O3 -I./include 

all: libshim.so

# libddb.a: hook.o logger.o monitor.o event.o tag.o
# 	$(AR) qc $@ $^

libshim.so: shim.o
	$(GCC) -shared -lpthread -ldl -o $@ $^

shim.o: shim.c
	$(GCC) -I./include -O1 -g -fno-omit-frame-pointer -fPIC -o $@ -c $<

# hook.o: hook.c
# 	$(CC) $(CFLAGS) -o $@ -c $<

# logger.o: logger.c
# 	$(CC) $(CFLAGS) -I./include -o $@ -c $<

# monitor.o: monitor.c
# 	$(CC) $(CFLAGS) -o $@ -c $<

# tag.o: tag.c
# 	$(CC) $(CFLAGS) -o $@ -c $<

# event.o: event.c
# 	$(CC) $(CFLAGS) -fPIC -o $@ -c $<

clean:
	rm shim.o libshim.so
