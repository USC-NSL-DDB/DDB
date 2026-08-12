#pragma once

#include <cstdint>

#include "cycle_counter.hpp"

class RealTimer {
public:
  static void init();

  static inline __attribute__((always_inline)) uint64_t elapsed() {
    return Cycles::toMicroseconds(Cycles::rdtsc() - start_);
  }

private:
  RealTimer();

  static uint64_t start_;
};