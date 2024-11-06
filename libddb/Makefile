GCC = /usr/bin/gcc
AR = /usr/bin/ar

CFLAGS = -g -O0 -I./include 

all: libshim.so test_bins/dummy

libshim.so: hook.o logger.o shim.o 
	$(GCC) -g -shared -fPIC -lpthread -ldl -o $@ $^

shim.o: shim.c
	$(GCC) $(CFLAGS) -fno-omit-frame-pointer -fPIC -o $@ -c $<

logger.o: logger.c
	$(GCC) $(CFLAGS) -fPIC -o $@ -c $<

hook.o: hook.c 
	$(GCC) $(CFLAGS) -fPIC -o $@ -c $<

test_bins/dummy: test_bins/dummy.c
	$(GCC) $(CFLAGS) -lpthread -o $@ $<

clean:
	rm shim.o libshim.so test_bins/dummy
