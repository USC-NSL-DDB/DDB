#pragma once

#include <iostream>
#include <cstring>

struct Args {
    bool enable_ddb = false ;
    bool wait_for_attach = true;
};

static inline void print_usage(const char* prog_name) {
    std::cout << "Usage: " << prog_name << " [options]\n"
              << "Options:\n"
              << "  --ddb               Enable DDB integration (default: false)\n"
              << "  --no-wait           Disable wait for attach (default: false)\n"
              << "  -h, --help          Show this help message\n";
}

static inline Args parse_args(int argc, char* argv[]) {
    Args args;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--ddb") == 0) {
            args.enable_ddb = true;
        }  else if (strcmp(argv[i], "--no-wait") == 0) {
            args.wait_for_attach = false;
        } else if (strcmp(argv[i], "-h") == 0 || strcmp(argv[i], "--help") == 0) {
            print_usage(argv[0]);
            exit(0);
        } else {
            std::cerr << "Unknown option: " << argv[i] << "\n";
            print_usage(argv[0]);
            exit(1);
        }
    }
    return args;
}