#include "real_timer.hpp"

uint64_t RealTimer::start_ = 0;

void RealTimer::init() {
    Cycles::init();
    RealTimer::start_ = Cycles::rdtsc();
}
