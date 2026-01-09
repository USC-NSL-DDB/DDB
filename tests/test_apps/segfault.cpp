// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <iostream>
#include <chrono>
#include <thread>
#include <ctime>
#include <cstring>

#include "ddb/integration.hpp"

struct Args {
    bool enable_ddb = false ;
    bool wait_for_attach = true;
};

void print_usage(const char* prog_name) {
    std::cout << "Usage: " << prog_name << " [options]\n"
              << "Options:\n"
              << "  --ddb               Enable DDB integration (default: false)\n"
              << "  --no-wait           Disable wait for attach (default: false)\n"
              << "  -h, --help          Show this help message\n";
}

Args parse_args(int argc, char* argv[]) {
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

timespec* get_current_time() {
    timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return &ts;
}

void sub_function_get_time() {
    [[maybe_unused]] timespec* current_time = get_current_time();
    current_time->tv_sec = 0; // Force a segmentation fault
    current_time->tv_nsec = 0;
    std::cout << "SHOULD CRASH RIGHT HERE!" << std::endl;
}

int main(int argc, char* argv[]) {
    Args args = parse_args(argc, argv);

    auto proc_alias = "segfault_app";

    if (args.enable_ddb) {
        auto cfg = DDB::Config::get_default("127.0.0.1")
                       .with_alias(proc_alias)
                       .with_logical_group(proc_alias);
        cfg.wait_for_attach = args.wait_for_attach;
        auto connector = DDB::DDBConnector(cfg);
        connector.init();
    }
    
    sub_function_get_time();
    return 0;
}
