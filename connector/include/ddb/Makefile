# Just a hack to generate compile_commands.json.
# Considering this is a header-only library, 
# the generated static library is not meaningful.

CXX = g++
CXXFLAGS = -std=c++17 -Wall -Wextra -g -I../

SRC_DIR = ./
SRCS = $(wildcard $(SRC_DIR)/*.c)
OBJS = $(SRCS:.c=.o)

TARGET = libdummy.a

all: $(TARGET)

# Generate compile_commands.json using bear
gen_compile_commands: clean
	bear -- $(MAKE) $(TARGET)
	rm -f $(OBJS) $(TARGET)

$(TARGET): $(OBJS)
	ar rcs $@ $^

%.o: %.c
	$(CXX) $(CXXFLAGS) -c $< -o $@

clean:
	rm -f $(OBJS) $(TARGET) compile_commands.json

.PHONY: all clean gen_compile_commands
