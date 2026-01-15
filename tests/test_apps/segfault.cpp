// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <iostream>
#include <ctime>
#include <cstring>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

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
