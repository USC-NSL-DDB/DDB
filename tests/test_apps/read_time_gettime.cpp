// Simple app to continuously read and print the current date and time
// File: read_time.cpp

#include <chrono>
#include <iostream>
#include <thread>
#include <time.h>

#include "ddb/integration.hpp"
#include "ddb_helper.hpp"

static const clockid_t clockid_for_gpr_clock[] = {CLOCK_MONOTONIC,
                                                  CLOCK_REALTIME};

int main(int argc, char *argv[]) {
  Args args = parse_args(argc, argv);

  // Parse clock source (default to 0 if not specified)
  int clock_source = 0;
  if (argc > 1) {
    for (int i = 1; i < argc; i++) {
      std::string arg = argv[i];
      if (arg == "--clock" || arg == "-c") {
        if (i + 1 < argc) {
          clock_source = std::atoi(argv[i + 1]);
          if (clock_source != 0 && clock_source != 1) {
            std::cerr
                << "Invalid clock source. Use 0 (MONOTONIC) or 1 (REALTIME)"
                << std::endl;
            clock_source = 0;
          }
        }
        break;
      }
    }
  }

  std::cout << "Using clock source: " << clock_source << " (";
  if (clock_source == 0) {
    std::cout << "CLOCK_MONOTONIC";
  } else {
    std::cout << "CLOCK_REALTIME";
  }
  std::cout << ")" << std::endl;

  auto proc_alias = "read_time_app";
  if (args.enable_ddb) {
    auto cfg = DDB::Config::get_default("127.0.0.1")
                   .with_alias(proc_alias)
                   .with_logical_group(proc_alias);
    cfg.wait_for_attach = args.wait_for_attach;
    auto connector = DDB::DDBConnector(cfg);
    connector.init();
  }

  struct timespec now;
  while (true) {
    clock_gettime(clockid_for_gpr_clock[clock_source], &now);
    if (clock_source == 0) {
        long long total_ns = now.tv_sec * 1000000000LL + now.tv_nsec;
        long long seconds = total_ns / 1000000000LL;
        long long milliseconds = (total_ns / 1000000LL) % 1000;
        long long microseconds = (total_ns / 1000LL) % 1000;
        long long nanoseconds = total_ns % 1000;
        
        std::cout << "CLOCK_MONOTONIC: " << seconds << "s " 
                << milliseconds << "ms " 
                << microseconds << "us " 
                << nanoseconds << "ns" << std::endl;
    } else {
        std::time_t realtime_sec = now.tv_sec;
        std::cout << "CLOCK_REALTIME: " << std::ctime(&realtime_sec);
    }
    std::this_thread::sleep_for(std::chrono::seconds(1));
  }
  return 0;
}
