// Reproducer for libfaketime + condition_variable::wait_for issue.

#include <chrono>
#include <condition_variable>
#include <iostream>
#include <mutex>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

int main(int argc, char* argv[]) {
    Args args = parse_args(argc, argv);
    auto proc_alias = "cv_wait_app";
    if (args.enable_ddb) {
        auto cfg = DDB::Config::get_default("127.0.0.1")
                       .with_alias(proc_alias)
                       .with_logical_group(proc_alias);
        cfg.wait_for_attach = args.wait_for_attach;
        auto connector = DDB::DDBConnector(cfg);
        connector.init();
    }

    std::mutex mtx;
    std::condition_variable cv;

    auto timeout = std::chrono::milliseconds(5000);

    std::cout << "Election timeout set to " << timeout.count() << " ms" << std::endl;

    for (int round = 1; round <= 20; ++round) {
        std::unique_lock<std::mutex> lock(mtx);

        std::cout << "Round " << round << ": Election Thread Before Sleep" << std::endl;

        auto status = cv.wait_for(lock, timeout);

        if (status == std::cv_status::timeout) {
            std::cout << "Round " << round << ": Election Thread After Sleep (timed out)" << std::endl;
        } else {
            std::cout << "Round " << round << ": Election Thread After Sleep (notified)" << std::endl;
        }
    }

    std::cout << "Done" << std::endl;
    return 0;
}
