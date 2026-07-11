// The application under test for the "PET perceived time gap" experiment.
//
// It runs a busy compute loop and, once per iteration, records a pair of
// timestamps:
//
//   perceived_us -- CLOCK_REALTIME (std::chrono::system_clock). This is what the
//                   application thinks the time is. libfaketime intercepts it and
//                   subtracts whatever offset DDB has written into the FAKETIME
//                   environment variable, so this is the *perceived* timeline.
//
//   real_us      -- rdtsc, via RealTimer. libfaketime cannot intercept an
//                   instruction, so this always advances with real wall-clock
//                   time, including while the process is stopped by the debugger.
//                   This is the ground-truth timeline.
//
// A debugger pause shows up as a jump in real_us. Whether it *also* shows up as a
// jump in perceived_us is exactly what the experiment measures: with faketime and
// a dynamically adjusted offset it should not, and the leftover jump is the
// perceived time gap we are after.
//
// The run length is measured with rdtsc, so it is a fixed amount of *real* time
// regardless of how much time the faketime offset hides.
//
// Knobs (environment variables):
//   PET_OUT          output CSV path                      (default ./samples.csv)
//   PET_DURATION_SEC how long to run, real seconds        (default 30)
//   PET_WORK_SIZE    inner loop length; sets sample rate  (default 50000)
//   PET_DDB_IP       IP the DDB connector reports itself on, and that DDB
//                    ssh'es back to in order to attach gdb (default 127.0.0.1)

#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <sched.h>
#include <string>
#include <thread>
#include <vector>

#include "real_timer.hpp"

#include "ddb/integration.hpp"

namespace {

// One sample. Padded to a cache line so the producer's stores never share a line
// with the consumer's loads.
struct alignas(64) TS {
  int64_t perceived_us; // CLOCK_REALTIME, faked by libfaketime
  uint64_t real_us;     // rdtsc since RealTimer::init(), never faked
};

// Single-producer / single-consumer lock-free ring. The worker must never block
// on the logger: a mutex or an allocation in the sampling path would show up in
// the measurement.
template <typename T, size_t Capacity> class RingBuffer {
public:
  bool push(const T &value) {
    const size_t head = head_.load(std::memory_order_relaxed);
    const size_t next_head = (head + 1) % Capacity;
    if (next_head == tail_.load(std::memory_order_acquire)) {
      return false; // full
    }
    data_[head] = value;
    head_.store(next_head, std::memory_order_release);
    return true;
  }

  bool pop(T &value) {
    const size_t tail = tail_.load(std::memory_order_relaxed);
    if (tail == head_.load(std::memory_order_acquire)) {
      return false; // empty
    }
    value = data_[tail];
    tail_.store((tail + 1) % Capacity, std::memory_order_release);
    return true;
  }

private:
  alignas(64) T data_[Capacity];
  alignas(64) std::atomic<size_t> head_{0};
  alignas(64) std::atomic<size_t> tail_{0};
};

constexpr size_t kRingCapacity = 1 << 16;

std::atomic<bool> g_done{false};
std::atomic<uint64_t> g_dropped{0};

// The application's own notion of "now". This is the call libfaketime hooks.
inline int64_t perceived_now_us() {
  return std::chrono::duration_cast<std::chrono::microseconds>(
             std::chrono::system_clock::now().time_since_epoch())
      .count();
}

size_t env_size_t(const char *name, size_t fallback) {
  const char *v = std::getenv(name);
  if (v == nullptr || *v == '\0') {
    return fallback;
  }
  return static_cast<size_t>(std::strtoull(v, nullptr, 10));
}

// Some work to keep the loop honest, so the compiler cannot hoist the whole
// thing away and the iteration takes a stable, non-trivial amount of time.
void compute(std::vector<uint64_t> &buffer) {
  for (size_t i = 0; i < buffer.size(); ++i) {
    buffer[i] = (buffer[i] / 2) * buffer[i] + 1;
  }
}

void worker(RingBuffer<TS, kRingCapacity> &rb, uint64_t duration_us,
            size_t work_size) {
  std::vector<uint64_t> values(work_size);
  for (size_t i = 0; i < work_size; ++i) {
    values[i] = i;
  }

  while (true) {
    compute(values);

    // Sample both clocks back to back. Order matters only to within the cost of
    // the two reads, which is tens of nanoseconds -- far below the pauses we are
    // measuring.
    const int64_t perceived = perceived_now_us();
    const uint64_t real = RealTimer::elapsed();

    if (!rb.push(TS{perceived, real})) {
      g_dropped.fetch_add(1, std::memory_order_relaxed);
    }

    // Real time, not perceived time: the run must last the same wall-clock
    // duration whether or not faketime is hiding the pauses.
    if (real >= duration_us) {
      break;
    }
  }
  g_done.store(true, std::memory_order_release);
}

void logger(RingBuffer<TS, kRingCapacity> &rb, const std::string &out_path) {
  FILE *out = std::fopen(out_path.c_str(), "w");
  if (out == nullptr) {
    std::cerr << "[pet] cannot open " << out_path << ": " << std::strerror(errno)
              << std::endl;
    std::exit(1);
  }
  std::fprintf(out, "perceived_us,real_us\n");

  const auto drain = [&]() {
    TS value;
    while (rb.pop(value)) {
      std::fprintf(out, "%lld,%llu\n",
                   static_cast<long long>(value.perceived_us),
                   static_cast<unsigned long long>(value.real_us));
    }
  };

  while (!g_done.load(std::memory_order_acquire)) {
    drain();
    // Yield rather than sleep: sleeping goes through nanosleep, which libfaketime
    // also intercepts. Nothing in the measurement path should depend on the
    // faked clock.
    sched_yield();
  }
  drain(); // whatever the worker pushed after it set g_done

  std::fclose(out);
}

} // namespace

int main() {
  const char *ip_env = std::getenv("PET_DDB_IP");
  const std::string ddb_ip = (ip_env && *ip_env) ? ip_env : "127.0.0.1";
  const std::string out_path = [] {
    const char *v = std::getenv("PET_OUT");
    return std::string((v && *v) ? v : "./samples.csv");
  }();
  const uint64_t duration_us = env_size_t("PET_DURATION_SEC", 30) * 1000000ULL;
  const size_t work_size = env_size_t("PET_WORK_SIZE", 50000);

  // Report ourselves to DDB and block until it has attached gdb and resumed us.
  // Everything below therefore runs under the debugger, which is what lets the
  // harness interrupt us.
  auto ddb_config = DDB::Config::get_default(ddb_ip)
                        .with_tag("pet")
                        .with_alias("faketime_pause");
  auto connector = DDB::DDBConnector(ddb_config);
  connector.init();

  // Calibrate the TSC only after the debugger has let go of us, so the attach
  // handshake is not part of the measured timeline.
  RealTimer::init();

  std::cerr << "[pet] running for " << duration_us / 1000000 << "s (real), "
            << "work_size=" << work_size << ", out=" << out_path << std::endl;

  static RingBuffer<TS, kRingCapacity> rb;
  std::thread w(worker, std::ref(rb), duration_us, work_size);
  std::thread l(logger, std::ref(rb), std::cref(out_path));
  w.join();
  l.join();

  const uint64_t dropped = g_dropped.load(std::memory_order_relaxed);
  if (dropped > 0) {
    std::cerr << "[pet] WARNING: dropped " << dropped
              << " samples (ring buffer full)" << std::endl;
  }
  std::cerr << "[pet] done" << std::endl;
  return 0;
}
