#include <iostream>
#include <chrono>
#include <thread>
#include <ctime>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

int main(int argc, char* argv[]) {
    Args args = parse_args(argc, argv);
    auto proc_alias = "basic_sleep_app";
    if (args.enable_ddb) {
        auto cfg = DDB::Config::get_default("127.0.0.1")
                       .with_alias(proc_alias)
                       .with_logical_group(proc_alias);
        cfg.wait_for_attach = args.wait_for_attach;
        auto connector = DDB::DDBConnector(cfg);
        connector.init();
    }
    
    while (true) {
        // Get current time with microsecond precision
        auto now = std::chrono::system_clock::now();
        auto now_time_t = std::chrono::system_clock::to_time_t(now);
        auto now_us = std::chrono::duration_cast<std::chrono::microseconds>(now.time_since_epoch()) % 1000000;

        // Convert to human-readable format
        char buf[64];
        std::strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M:%S", std::localtime(&now_time_t));

        // Output timestamp with microseconds
        std::cout << "Current time: " << buf << "." << std::setfill('0') << std::setw(6) << now_us.count() << std::endl;

        // Sleep for 5 seconds
        std::this_thread::sleep_for(std::chrono::seconds(5));
    }
    return 0;
}