#pragma once

#include <map>
#include <vector>

#include "common/common.hpp"
#include "common/config.hpp"

namespace toolings {
struct RaftInstanceConfig {
  uint64_t id;
  uint64_t port;
  std::string listening_addr;
  std::string external_addr;
};

struct ConfigGen {
  static std::vector<RaftInstanceConfig>
  gen_local_instances(uint64_t num_instances, uint64_t start_port) {
    std::vector<RaftInstanceConfig> instances;
    for (uint64_t i = 0; i < num_instances; i++) {
      RaftInstanceConfig instance;
      instance.id = static_cast<uint64_t>(i);
      instance.port = start_port + i;
      instance.listening_addr = "localhost:" + std::to_string(instance.port);
      instance.external_addr = instance.listening_addr;
      instances.emplace_back(instance);
    }
    return instances;
  }

  static std::vector<RaftInstanceConfig>
  gen_net_instances(const std::vector<std::string> &addrs,
                    uint64_t listening_port) {
    std::vector<RaftInstanceConfig> instances;
    for (uint64_t i = 0; i < addrs.size(); i++) {
      RaftInstanceConfig instance;
      instance.id = static_cast<uint64_t>(i);
      instance.listening_addr = "0.0.0.0:" + std::to_string(listening_port);
      instance.external_addr = addrs[i] + ":" + std::to_string(listening_port);
      instances.emplace_back(instance);
    }
    return instances;
  }

  static std::vector<rafty::Config>
  gen_raft_configs(const std::vector<RaftInstanceConfig> &instances) {
    std::vector<rafty::Config> configs;
    for (const auto &instance : instances) {
      rafty::Config config;
      config.id = instance.id;
      config.addr = instance.listening_addr;
      std::map<uint64_t, std::string> peers;
      for (const auto &peer : instances) {
        if (peer.id != instance.id) {
          peers[peer.id] = peer.external_addr;
        }
      }
      config.peer_addrs = peers;
      configs.emplace_back(config);
    }
    return configs;
  }
};

} // namespace toolings
