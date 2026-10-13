#pragma once

#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#if defined(__linux__)
#include <sys/prctl.h>
#endif

#include <ddb/integration.hpp>
#include "absl/flags/flag.h"

ABSL_FLAG(bool, ddb, false, "Enable DDB automatic discovery and attach.");
ABSL_FLAG(std::string, ddb_host_ip, "127.0.0.1", "DDB service address.");
ABSL_FLAG(std::string, ddb_proc_alias, "greeter", "DDB process alias.");

inline std::unique_ptr<DDB::DDBConnector> start_ddb() {
  if (!absl::GetFlag(FLAGS_ddb)) return nullptr;
#if defined(__linux__)
  // DDB discovers this process independently. Permit its same-user debugger
  // without changing the CI host's Yama policy for unrelated processes.
  if (prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY) != 0) {
    throw std::runtime_error("allow DDB debugger attach: PR_SET_PTRACER failed");
  }
#endif
  auto config = DDB::Config::get_default(absl::GetFlag(FLAGS_ddb_host_ip))
                    .with_alias(absl::GetFlag(FLAGS_ddb_proc_alias));
  auto connector = std::make_unique<DDB::DDBConnector>(config);
  std::cerr << "DDB automatic attach requested for "
            << absl::GetFlag(FLAGS_ddb_proc_alias) << std::endl;
  connector->init();
  std::cerr << "DDB automatic attach completed for "
            << absl::GetFlag(FLAGS_ddb_proc_alias) << std::endl;
  return connector;
}
