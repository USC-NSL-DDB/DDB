#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <fcntl.h>
#include <gtest/gtest.h>
#include <optional>
#include <sys/wait.h>
#include <unistd.h>
#include <unordered_map>

#include "absl/flags/flag.h"
#include "absl/flags/parse.h"

#include "spdlog/sinks/basic_file_sink.h"
#include "spdlog/sinks/stdout_color_sinks.h"
#include "spdlog/sinks/stdout_sinks.h"
#include "spdlog/spdlog.h"

#include "toolings/config_gen.hpp"
#include "toolings/test_config.hpp"

using namespace toolings;

const std::string NODE_APP_PATH = "../app/node";

// ABSL_FLAG(bool, verbose, false, "if enable default logging");
ABSL_FLAG(int, verbosity, 0,
          "Verbosity level: 0 (silent), 1 (display tester message), 2 (display "
          "tester + raft message (file sink only)), 3 (all message, file sink "
          "+ console sink)");

static constexpr std::chrono::milliseconds TIMEOUT(1000);
static constexpr std::string tester_logger_name = "tester";

static void DisableConsoleLogging(spdlog::logger &logger,
                                  bool disable_file_sink = false) {
  // disable console logging
  // file sink won't be affected
  for (auto &sink : logger.sinks()) {
    if (auto console_sink =
            std::dynamic_pointer_cast<spdlog::sinks::stdout_sink_mt>(sink)) {
      console_sink->set_level(spdlog::level::off);
    } else if (auto console_sink = std::dynamic_pointer_cast<
                   spdlog::sinks::stdout_color_sink_mt>(sink)) {
      console_sink->set_level(spdlog::level::off);
    } else if (auto file_sink =
                   std::dynamic_pointer_cast<spdlog::sinks::basic_file_sink_mt>(
                       sink)) {
      if (disable_file_sink)
        file_sink->set_level(spdlog::level::off);
    }
  }
}

// Helper class to suppress output
class OutputSuppressor {
public:
  OutputSuppressor() = default;
  ~OutputSuppressor() = default;

  void Enable() {
    // devnull.open("/dev/null");
    // old_buf = std::cout.rdbuf(devnull.rdbuf());

    // Open /dev/null
    devnull_fd = open("/dev/null", O_WRONLY);
    if (devnull_fd == -1) {
      throw std::runtime_error("Failed to open /dev/null");
    }

    // Save the original stdout file descriptor
    saved_stdout_fd = dup(STDOUT_FILENO);
    if (saved_stdout_fd == -1) {
      throw std::runtime_error("Failed to duplicate stdout file descriptor");
    }

    // Redirect stdout to /dev/null
    if (dup2(devnull_fd, STDOUT_FILENO) == -1) {
      throw std::runtime_error("Failed to redirect stdout to /dev/null");
    }
  }

  void Disable() {
    // Restore the original stdout
    if (dup2(saved_stdout_fd, STDOUT_FILENO) == -1) {
      std::cerr << "Failed to restore stdout" << std::endl;
    }

    // Close file descriptors
    close(devnull_fd);
    close(saved_stdout_fd);
  }

private:
  int devnull_fd;
  int saved_stdout_fd;
};

static std::shared_ptr<spdlog::logger>
get_or_create_logger(const std::string &logger_name) {
  // Try to get an existing logger
  auto logger = spdlog::get(logger_name);
  // If the logger doesn't exist, create it
  if (!logger) {
    logger = spdlog::stdout_color_mt(logger_name);
  }
  return logger;
}

static std::shared_ptr<spdlog::logger> logger =
    get_or_create_logger(tester_logger_name);

class RaftTest : public ::testing::Test {
protected:
  void SetUp() override {
    int verbosity = absl::GetFlag(FLAGS_verbosity);
    if (verbosity < 1)
      DisableConsoleLogging(*spdlog::get("tester"));
    this->verbosity = std::max(1, verbosity);
    spdlog::flush_every(std::chrono::seconds(3));
    // this->suppressor.Enable();
  }

  void TearDown() override {
    // Optionally re-enable if needed
    // this->suppressor.Disable();
  }

  int verbosity = 0;
  OutputSuppressor suppressor;
};

TEST_F(RaftTest, InitialElectionA) {
  try {
    auto local_confs = ConfigGen::gen_local_instances(3, 50051);
    auto r_confs = ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(
      r_confs, NODE_APP_PATH, 0, verbosity - 1,
      "InitialElectionA"
    );

    cfg.begin();

    // is a leader elected?
    cfg.check_one_leader();
    logger->info("waiting for initial election to finish");

    std::this_thread::sleep_for(std::chrono::milliseconds(100));
    auto term1 = cfg.check_terms();

    ASSERT_TRUE(term1.has_value())
        << "Failed to get terms after first election";
    ASSERT_TRUE(term1.value() >= 1)
        << "term is " << term1.value() << ", but should be at least 1";

    // does the leader+term stay the same if there is no network failure?
    logger->info("start to waiting for a while... no network failure - leader "
                 "and term should stay the same");
    std::this_thread::sleep_for(2 * TIMEOUT);
    auto term2 = cfg.check_terms();
    ASSERT_TRUE(term2.has_value())
        << "Failed to get terms after second election";
    if (term2.value() != term1.value()) {
      GTEST_LOG_(WARNING) << "warning: term changed from " << term1.value()
                          << " to " << term2.value()
                          << ". Potential unstable election.";
    }
    cfg.check_one_leader();
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, ReElectionA) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "ReElectionA");

    cfg.begin();

    auto leader1 = cfg.check_one_leader();
    EXPECT_TRUE(leader1.has_value()) << "No leader found!";

    logger->info("Disconnect leader - force reelection");
    cfg.disconnect(*leader1);
    cfg.check_one_leader();
    logger->info("Disconnect leader finished - should done election.");

    logger->info("Reconnect leader - should have no reelection");
    cfg.reconnect(*leader1);
    auto leader2 = cfg.check_one_leader();
    EXPECT_TRUE(leader2.has_value()) << "No leader found!";
    logger->info("Reconnect leader finished - should have no election.");

    logger->info("Disconnect two servers - should have no leader.");
    cfg.disconnect((*leader2 + 1) % servers);
    cfg.disconnect(*leader2);
    std::this_thread::sleep_for(2 * TIMEOUT);
    cfg.check_no_leader();
    logger->info("Disconnect two servers finished - should have no leader.");

    logger->info("Reconnect two servers - should trigger one election.");
    cfg.reconnect((*leader2 + 1) % servers);
    cfg.check_one_leader();
    logger->info(
        "Reconnect two servers finished - should trigger one election.");

    cfg.reconnect(*leader2);
    cfg.check_one_leader();
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, ManyElectionA) {
  try {
    auto servers = 7;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "ManyElectionA");

    cfg.begin();

    cfg.check_one_leader();

    auto iters = 5;
    for (int i = 0; i < iters; i++) {
      auto ids = cfg.pick_n_servers(3);
      for (auto id : ids) {
        cfg.disconnect(id);
        logger->info("Disconnect server {}", id);
      }

      // either the current leader should still be alive,
      // or the remaining four should elect a new one.
      cfg.check_one_leader();

      for (auto id : ids) {
        cfg.reconnect(id);
        logger->info("Reconnect server {}", id);
      }
    }

    cfg.check_one_leader();
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, BasicAgreeB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "BasicAgreeB");

    cfg.begin();

    auto iters = 3;
    for (int i = 1; i < iters + 1; i++) {
      auto cr = cfg.n_committed(i);
      ASSERT_FALSE(cr.num > 0) << "some have committed before any proposal";

      auto data = std::to_string(i * 100);
      logger->info("Propose {}", data);
      auto xindex = cfg.one(data, servers, false);
      ASSERT_TRUE(xindex.has_value()) << "no valid committed index. might "
                                         "return the wrong index or fail to "
                                         "commit logs.";
      ASSERT_TRUE(*xindex == static_cast<uint64_t>(i))
          << "got index " << *xindex << ", but want " << i;
    }
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, ManyAgreeB) {
  try {
    auto servers = 5;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "ManyAgreeB");

    cfg.begin();

    auto iters = 50;
    for (int i = 1; i < iters + 1; i++) {
      auto cr = cfg.n_committed(i);
      ASSERT_FALSE(cr.num > 0) << "some have committed before any proposal";

      auto data = std::to_string(i * 100);
      logger->info("Propose {}", data);
      auto xindex = cfg.one(data, servers, false);
      ASSERT_TRUE(xindex.has_value()) << "no valid committed index. might "
                                         "return the wrong index or fail to "
                                         "commit logs.";
      ASSERT_TRUE(*xindex == static_cast<uint64_t>(i))
          << "got index " << *xindex << ", but want " << i;
    }
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, RPCBytesB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "RPCBytesB");

    cfg.begin();

    auto one1 = cfg.one(std::to_string(99), servers, false);
    ASSERT_TRUE(one1.has_value());
    auto bytes0 = cfg.get_rpc_total_bytes();

    logger->info(
        "Start to proposing data. Each proposal (an complete agreement) "
        "should transfer about (# of servers - 1) * bytes(data). Some "
        "additional bytes will be tolerated.");

    auto iters = 10;
    uint64_t sent = 0;
    for (int index = 2; index < iters + 2; index++) {
      auto rand_str = toolings::generate_random_string(1000);
      logger->info("Propose a command/data ({} bytes)", rand_str.size());
      auto xindex = cfg.one(rand_str, servers, false);
      ASSERT_TRUE(xindex.has_value()) << "no valid committed index. might "
                                         "return the wrong index or fail to "
                                         "commit logs.";
      ASSERT_TRUE(xindex.value() == static_cast<uint64_t>(index))
          << "got index " << xindex.value() << ", but expect " << index;
      sent += rand_str.size();
    }

    auto bytes1 = cfg.get_rpc_total_bytes();
    auto got = bytes1 - bytes0;
    auto expected = static_cast<uint64_t>(servers - 1) * sent;
    expected += static_cast<uint64_t>(iters) *
                1000; // being leinient here for padding extra 1000 bytes per
                      // agreement here.
    // You shouldn't need that much additional bytes for each agreement.

    // std::cout << "bytes sent (grpc): " << got << "; actual sent bytes = " <<
    // expected << std::endl;
    ASSERT_FALSE(got > expected)
        << "too many RPC bytes; got " << got << " bytes, but expect "
        << expected << " bytes";
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, FailAgreeB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "FailAgreeB");

    cfg.begin();

    logger->info("Start initial agreement.");
    auto one1 = cfg.one("101", servers, false);
    ASSERT_TRUE(one1.has_value());
    logger->info("Finished initial agreement.");

    // disconnect one follower from the network
    auto leader = cfg.check_one_leader();
    ASSERT_TRUE(leader.has_value()) << "No leader found!";
    auto id = cfg.pick_n_servers(1, leader.value()).front();
    logger->info("Start disconnecting server {}.", id);
    cfg.disconnect(id);
    logger->info("Disconnected server {}.", id);
    logger->info("Start checking agreement after disconnection.");

    std::this_thread::sleep_for(TIMEOUT / 2);
    // the leader and remaining follower should be
    // able to agree despite the disconnected follower.
    one1 = cfg.one("102", servers - 1, false);
    ASSERT_TRUE(one1.has_value());
    one1 = cfg.one("103", servers - 1, false);
    ASSERT_TRUE(one1.has_value());
    logger->info("Should agree after disconnection.");
    std::this_thread::sleep_for(TIMEOUT);
    one1 = cfg.one("104", servers - 1, false);
    ASSERT_TRUE(one1.has_value());
    one1 = cfg.one("105", servers - 1, false);
    ASSERT_TRUE(one1.has_value());

    logger->info("Reconnect");
    // reconnect
    cfg.reconnect(id);

    std::this_thread::sleep_for(
        TIMEOUT); // give time for reconnection and recovery
    // the full set of servers should preserve
    // previous agreements, and be able to agree
    // on new commands.
    one1 = cfg.one("106", servers, false);
    ASSERT_TRUE(one1.has_value());
    std::this_thread::sleep_for(TIMEOUT);
    one1 = cfg.one("107", servers, false);
    ASSERT_TRUE(one1.has_value());
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

// TODO: remove for hidden test case
TEST_F(RaftTest, FailNoAgreeExtraB) {
  try {
    auto servers = 5;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "FailNoAgreeExtraB");

    cfg.begin();

    auto one1 = cfg.one("10", servers, false);
    ASSERT_TRUE(one1.has_value());

    // 3 of 5 followers disconnect
    auto leader = cfg.check_one_leader();
    ASSERT_TRUE(leader.has_value()) << "No leader found!";
    auto ids = cfg.pick_n_servers(3, leader.value());
    for (auto id : ids) {
      logger->info("Disconnect server {}.", id);
      cfg.disconnect(id);
    }

    auto r = cfg.propose(leader.value(), "20");
    ASSERT_TRUE(r.is_leader) << "leader rejected proposal";
    ASSERT_TRUE(r.index == 2) << "expected index 2, got " << r.index;

    std::this_thread::sleep_for(2 * TIMEOUT);

    auto cr = cfg.n_committed(r.index);
    ASSERT_FALSE(cr.num > 0)
        << cr.num
        << " servers have committed, which shouldn't happen "
           "as the quorum (majority) cannot be established.";

    for (auto id : ids) {
      logger->info("Reconnect server {}.", id);
      cfg.reconnect(id);
    }

    // the disconnected majority may have chosen a leader from
    // among their own ranks, forgetting index 2.
    auto leader2 = cfg.check_one_leader();
    ASSERT_TRUE(leader2.has_value()) << "No leader found!";
    auto r1 = cfg.propose(leader2.value(), "30");
    ASSERT_TRUE(r1.is_leader) << "leader rejected proposal";

    ASSERT_FALSE(r1.index < 2 || r1.index > 3)
        << "unexpected index " << r1.index;

    one1 = cfg.one("1000", servers, false);
    ASSERT_TRUE(one1.has_value());
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, ConcurrentStartsB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "ConcurrentStartsB");

    cfg.begin();

    auto success = false;

    for (int try_ = 0; try_ < 5; try_++) {
      if (try_ > 0) {
        // wait for solution to settle
        std::this_thread::sleep_for(std::chrono::seconds(3));
      }

      auto leader = cfg.check_one_leader();
      ASSERT_TRUE(leader.has_value()) << "No leader found!";
      auto r = cfg.propose(leader.value(), "1");
      auto term = r.term;
      if (!r.is_leader) {
        // leader moved on really quickly
        continue;
      }

      auto iters = 5;
      std::vector<std::future<std::optional<uint64_t>>> is_fut;
      for (int ii = 0; ii < iters; ii++) {
        is_fut.push_back(std::async(std::launch::async,
                                    [&cfg, leader_id = leader.value(), term,
                                     ii]() -> std::optional<uint64_t> {
                                      auto r_ = cfg.propose(
                                          leader_id, std::to_string(100 + ii));
                                      if (r_.term != term) {
                                        return std::nullopt;
                                      }
                                      if (!r_.is_leader) {
                                        return std::nullopt;
                                      }
                                      return r_.index;
                                    }));
      }

      std::vector<uint64_t> is;
      for (auto &f : is_fut) {
        auto i = f.get();
        if (i)
          is.push_back(*i);
      }

      auto term_changed = false;
      auto states = cfg.ctrl->get_all_states();
      for (auto const &state : states) {
        // auto state = raft->get_state();
        if (state.term() != term) {
          // term changed -- can't expect low RPC counts
          term_changed = true;
          break;
        }
      }

      if (term_changed) {
        continue;
      }

      auto failed = false;
      std::vector<std::string> cmds;
      for (const auto &index : is) {
        auto wr = cfg.wait(index, servers, term);
        if (!wr.has_value()) {
          failed = true;
          break;
        }
        cmds.push_back(wr.value());
      }

      if (failed) {
        continue;
      }

      for (int ii = 0; ii < iters; ii++) {
        auto x = 100 + ii;
        auto ok = false;
        for (const auto &cmd : cmds) {
          if (cmd == std::to_string(x)) {
            ok = true;
          }
        }
        ASSERT_TRUE(ok) << "cmd " << x << " missing";
      }

      success = true;
      break;
    }

    ASSERT_TRUE(success) << "term changed too often";
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, RejoinB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "RejoinB");

    cfg.begin();

    auto one1 = cfg.one("101", servers, true);
    ASSERT_TRUE(one1.has_value());

    // leader network failure
    auto leader1 = cfg.check_one_leader();
    ASSERT_TRUE(leader1.has_value()) << "No leader found!";
    logger->info("Disconnect server {}.", leader1.value());
    cfg.disconnect(leader1.value());

    // make old leader try to agree on some entries
    cfg.propose(leader1.value(), "102");
    cfg.propose(leader1.value(), "103");
    cfg.propose(leader1.value(), "104");

    // new leader commits, also for index=2
    one1 = cfg.one("103", 2, true);
    ASSERT_TRUE(one1.has_value());

    // new leader network failure
    auto leader2 = cfg.check_one_leader();
    ASSERT_TRUE(leader2.has_value()) << "No leader found!";
    logger->info("Disconnect server {}.", leader2.value());
    cfg.disconnect(leader2.value());

    // older leader connected again
    logger->info("Reconnect server {}.", leader1.value());
    cfg.reconnect(leader1.value());

    one1 = cfg.one("104", 2, true);
    ASSERT_TRUE(one1.has_value());

    // all together now
    cfg.reconnect(leader2.value());

    one1 = cfg.one("105", servers, true);
    ASSERT_TRUE(one1.has_value());
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

// TODO: remove for hidden test case
TEST_F(RaftTest, BackupExtraB) {
  try {
    auto servers = 5;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "BackupExtraB");

    cfg.begin();

    auto one1 = cfg.one(generate_random_string(10), servers, true);
    ASSERT_TRUE(one1.has_value());

    // put leader and one follower in a partition
    auto leader1 = cfg.check_one_leader();
    ASSERT_TRUE(leader1.has_value()) << "No leader found!";
    auto ids = cfg.pick_n_servers(3, leader1.value());
    for (auto id : ids) {
      logger->info("Disconnect server {}.", id);
      cfg.disconnect(id);
    }

    std::this_thread::sleep_for(TIMEOUT / 2);

    // submit lots of commands that won't commit
    for (int i = 0; i < 50; i++) {
      cfg.propose(leader1.value(), generate_random_string(10));
    }

    std::this_thread::sleep_for(TIMEOUT / 2);

    std::set<uint64_t> exclude_ids;
    for (auto id : ids) {
      exclude_ids.insert(id);
    }
    auto remaining_ids = cfg.pick_n_servers(2, exclude_ids);
    for (auto id : remaining_ids) {
      logger->info("Disconnect server {}.", id);
      cfg.disconnect(id);
    }

    // allow other partition to recover
    for (auto id : ids) {
      logger->info("Reconnect server {}.", id);
      cfg.reconnect(id);
    }

    for (int i = 0; i < 50; i++) {
      one1 = cfg.one(generate_random_string(10), 3, true);
      ASSERT_TRUE(one1.has_value());
    }

    // now another partitioned leader and one follower
    auto leader2 = cfg.check_one_leader();
    ASSERT_TRUE(leader2.has_value()) << "No leader found!";
    auto other = cfg.pick_n_servers(1, leader2.value())[0];
    cfg.disconnect(other);

    // lots of successful commands to new group
    for (int i = 0; i < 50; i++) {
      cfg.propose(leader2.value(), generate_random_string(10));
    }

    std::this_thread::sleep_for(TIMEOUT / 2);

    // bring original leader back to life
    cfg.disconnect_all();

    for (auto const &r_id : remaining_ids) {
      logger->info("Reconnect server {}.", r_id);
      cfg.reconnect(r_id);
    }
    cfg.reconnect(other);

    std::this_thread::sleep_for(TIMEOUT / 2);
    // lots of successful commands to new group
    for (int i = 0; i < 50; i++) {
      one1 = cfg.one(generate_random_string(10), 3, true);
      ASSERT_TRUE(one1.has_value());
    }

    // now everyone
    cfg.reconnect_all();

    std::this_thread::sleep_for(TIMEOUT / 2);
    one1 = cfg.one(generate_random_string(10), servers, true);
    ASSERT_TRUE(one1.has_value());
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, AgreeOnSplitBrainExtraB) {
  try {
    auto servers = 5;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 1, verbosity - 1,
                                      "AgreeOnSplitBrainExtraB");

    cfg.begin();

    // Propose initial log entries
    for (int i = 1; i <= 5; ++i) {
      auto data = "entry" + std::to_string(i);
      logger->info("Propose {}", data);
      auto xindex = cfg.one(data, servers, false);
      ASSERT_TRUE(xindex.has_value()) << "Failed to commit log entry " << data;
      ASSERT_EQ(*xindex, static_cast<uint64_t>(i)) << "Unexpected log index for " << data;
    }

    // 3 of 5 followers disconnect
    auto leader = cfg.check_one_leader();
    ASSERT_TRUE(leader.has_value()) << "No leader found!";
    auto ids = cfg.pick_n_servers(3, leader.value());
    for (auto id : ids) {
      logger->info("Disconnect server {}.", id);
      cfg.disconnect(id);
    }

    for (int i = 6; i <= 10; i++) {
      auto data = "entry_old_leader_" + std::to_string(i);
      logger->info("Propose to old leader: {}", data);
      auto r = cfg.propose(leader.value(), data);
      ASSERT_TRUE(r.is_leader) << "leader rejected proposal";
      ASSERT_TRUE(r.index == (uint64_t)i) << "expected index " << i << ", got " << r.index;
    }

    std::this_thread::sleep_for(2 * TIMEOUT);

    for (int i = 6; i <= 10; i++) {
      auto cr = cfg.n_committed(i);
      ASSERT_FALSE(cr.num > 0)
        << cr.num
        << " servers have committed, which shouldn't happen "
           "as the quorum (majority) cannot be established from old partition.";
    }

    // form a new partition
    auto all_servers = cfg.pick_n_servers(servers);
    std::unordered_map<uint64_t, bool> new_partition;
    for (auto id : all_servers) {
      new_partition[id] = false;
    }
    for (auto id : ids) {
      new_partition[id] = true;
    }

    std::unordered_map<uint64_t, std::string> new_data_to_commit;
    std::optional<uint64_t> new_leader;
    for (int i = 6; i <= 10; i++) {
      auto data = "entry_on_new_partition_" + std::to_string(i);
      new_data_to_commit[i] = data;
      auto rs = cfg.ctrl->propose_to_all(data, new_partition);
      for (auto &r : rs) {
        if (r.is_leader()) {
          auto lid = r.id();
          if (!new_leader.has_value()) {
            new_leader = lid;
          } else {
            ASSERT_EQ(new_leader.value(), lid) << "Multiple leaders in the new partition";
          }
          auto idx = r.index();
          ASSERT_EQ(idx, (uint64_t)i) << "Unexpected log index (" << idx << ") for " << data;
        }
      }
    }

    // wait a while for the new partition to commit.
    std::this_thread::sleep_for(2 * TIMEOUT);

    // reconnect another partition.
    for (auto id : ids) {
      logger->info("Reconnect server {}.", id);
      cfg.reconnect(id);
    }

    // wait a while for the all to commit.
    std::this_thread::sleep_for(2 * TIMEOUT);

    for (auto &[idx, data] : new_data_to_commit) {
      auto cr = cfg.n_committed(idx);
      ASSERT_TRUE(cr.num = servers) << "Not all servers commit index " << idx << ", data: " << data;
    }

    // the disconnected majority may have chosen a leader from
    // among their own ranks, forgetting index 2.
    auto leader2 = cfg.check_one_leader();
    ASSERT_TRUE(leader2.has_value()) << "No leader found!";
    auto r1 = cfg.propose(leader2.value(), "1000");
    ASSERT_TRUE(r1.is_leader) << "leader rejected proposal";

    auto one1 = cfg.one("2000", servers, false);
    ASSERT_TRUE(one1.has_value());
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

TEST_F(RaftTest, RPCCountB) {
  try {
    auto servers = 3;
    auto local_confs = toolings::ConfigGen::gen_local_instances(servers, 50051);
    auto r_confs = toolings::ConfigGen::gen_raft_configs(local_confs);

    toolings::MultiprocTestConfig cfg(r_confs, NODE_APP_PATH, 0, verbosity - 1,
                                      "RPCCountB");

    cfg.begin();

    auto leader = cfg.check_one_leader();

    auto total1 = cfg.get_rpc_total_count();

    ASSERT_FALSE(total1 > 35 || total1 < 1)
        << "too many or few RPCs (" << total1 << ") to elect initial leader";

    size_t total2 = 0;
    auto success = false;

    for (int try_ = 0; try_ < 5; try_++) {
      if (try_ > 0) {
        // wait for solution to settle
        std::this_thread::sleep_for(std::chrono::seconds(3));
      }

      leader = cfg.check_one_leader();
      ASSERT_TRUE(leader.has_value()) << "No leader found!";
      total1 = cfg.get_rpc_total_count();

      auto iters = 10;
      auto p = cfg.propose(leader.value(), "1");
      if (!p.is_leader) {
        // leader moved on really quickly
        continue;
      }
      auto starti = p.index;
      auto term = p.term;

      auto term_changed = false;
      auto cmds = std::vector<std::string>();
      for (int i = 1; i < iters + 2; i++) {
        auto x = generate_random_string(10);
        cmds.emplace_back(x);
        auto p_ = cfg.propose(leader.value(), x);
        auto index1 = p_.index;
        if (p_.term != term) {
          term_changed = true;
          break;
        }
        if (!p_.is_leader) {
          term_changed = true;
          break;
        }
        ASSERT_FALSE(starti + i != index1) << "propose failed";
      }
      if (term_changed) {
        continue;
      }

      for (int i = 1; i < iters + 1; i++) {
        auto wr = cfg.wait(starti + i, servers, term);
        if (!wr.has_value()) {
          term_changed = true;
          break;
        }
        ASSERT_TRUE(wr.value() == cmds[i - 1])
            << "wrong value " << wr.value() << " committed for index "
            << starti + i;
        if (wr.value() != cmds[i - 1]) {
        }
      }
      if (term_changed) {
        continue;
      }

      auto failed = false;
      total2 = 0;
      auto states = cfg.ctrl->get_all_states();
      for (const auto &state : states) {
        if (state.term() != term) {
          // term changed -- can't expect low RPC counts
          // need to keep going to update total2
          failed = true;
        }
      }
      total2 = cfg.get_rpc_total_count();

      if (failed) {
        continue;
      }

      ASSERT_TRUE(static_cast<uint64_t>(total2 - total1) <=
                  static_cast<uint64_t>((iters + 1 + 3) * 3))
          << "too many RPCs (" << total2 - total1 << ") for " << iters
          << " entries";
      success = true;
      break;
    }

    ASSERT_TRUE(success) << "term changed too often";
    std::this_thread::sleep_for(TIMEOUT);

    auto total3 = cfg.get_rpc_total_count();

    // 3 * 100 tolerate a heartbeat interval as low as 10ms.
    ASSERT_TRUE(total3 - total2 <= 3 * 100)
        << "too many RPCs (" << total3 - total2 << ") for 1 second of idleness";
  } catch (const std::exception &e) {
    logger->error("Exception: {}", e.what());
    FAIL() << "Exception: " << e.what();
  }
}

static pid_t pgid = 0;

void signal_handler(int signal) {
  if (signal == SIGINT) {
    std::cout << "Caught SIGINT (Ctrl+C), cleaning up..." << std::endl;
    if (::kill(-pgid, SIGKILL) == 0) {
      // well... dead...
    } else {
      std::perror("Failed to kill process");
      std::exit(1);
    }
    exit(0);
  }
}

int main(int argc, char **argv) {
  ::testing::InitGoogleTest(&argc, argv);
  absl::ParseCommandLine(argc, argv);

  pgid = getpid();
  // Register the signal handler for Ctrl+C (SIGINT)
  struct sigaction sigIntHandler;
  sigIntHandler.sa_handler = signal_handler;
  sigemptyset(&sigIntHandler.sa_mask);
  sigIntHandler.sa_flags = 0;
  sigaction(SIGINT, &sigIntHandler, nullptr);

  return RUN_ALL_TESTS();
}
