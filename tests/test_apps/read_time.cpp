// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <iostream>
#include <chrono>
#include <thread>
#include <ctime>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

int main(int argc, char* argv[]) {
    Args args = parse_args(argc, argv);
    auto proc_alias = "read_time_app";
    if (args.enable_ddb) {
        auto cfg = DDB::Config::get_default("127.0.0.1")
                       .with_alias(proc_alias)
                       .with_logical_group(proc_alias);
        cfg.wait_for_attach = args.wait_for_attach;
        auto connector = DDB::DDBConnector(cfg);
        connector.init();
    }

    while (true) {
        auto now = std::chrono::system_clock::now();
        std::time_t now_time = std::chrono::system_clock::to_time_t(now);
        std::cout << std::ctime(&now_time);
        std::this_thread::sleep_for(std::chrono::seconds(1));
    }
    return 0;
}
