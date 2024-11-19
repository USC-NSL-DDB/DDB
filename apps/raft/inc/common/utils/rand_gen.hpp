#pragma once

#include <cstdint>
#include <random>

namespace rafty {
namespace utils {
// Thread-safe random number generator class
// TODO: double-check if thread-local would be sufficient
class RandGen {
public:
  uint64_t intn(uint64_t n) {
    // std::lock_guard<std::mutex> lock(mtx);
    std::uniform_int_distribution<int> dist(0, n - 1);
    return static_cast<uint64_t>(dist(rng));
  }

  static RandGen &get_instance() {
    static thread_local RandGen instance = RandGen();
    // static RandGen instance = RandGen();
    return instance;
  }

private:
  RandGen() : rng(std::random_device{}()) {}
  ~RandGen() = default;
  RandGen(const RandGen &) = delete;
  RandGen &operator=(const RandGen &) = delete;

private:
  std::mt19937_64 rng;
  // std::mutex mtx;
};
} // namespace utils
} // namespace rafty