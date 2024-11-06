# ROOT_PATH=..
# CXX = $(ROOT_PATH)/build/bin/clang++
# CC = $(ROOT_PATH)/build/bin/clang
GCC = /usr/bin/gcc
AR = /usr/bin/ar

CFLAGS = -g -O3 -I./include 

all: libshim.so test_bins/dummy

# libddb.a: hook.o logger.o monitor.o event.o tag.o
# 	$(AR) qc $@ $^

libshim.so: hook.o logger.o shim.o 
	$(GCC) -g -shared -fPIC -lpthread -ldl -o $@ $^

shim.o: shim.c
	$(GCC) -I./include -O0 -g -fno-omit-frame-pointer -fPIC -o $@ -c $<

logger.o: logger.c
	$(GCC) -I./include -O0 -g -fPIC -o $@ -c $<

hook.o: hook.c 
	$(GCC) -I./include -O0 -g -fPIC -o $@ -c $<

# test_bins/dummy: test_bins/dummy.c hook.o
# 	$(GCC) -I./include -g -O0 -o $@ $< -Wl,--whole-archive hook.o -Wl,--no-whole-archive

test_bins/dummy: test_bins/dummy.c
	$(GCC) -I./include -g -O0 -lpthread -o $@ $<


# monitor.o: monitor.c
# 	$(CC) $(CFLAGS) -o $@ -c $<

# tag.o: tag.c
# 	$(CC) $(CFLAGS) -o $@ -c $<

# event.o: event.c
# 	$(CC) $(CFLAGS) -fPIC -o $@ -c $<

clean:
	rm shim.o libshim.so test_bins/dummy
