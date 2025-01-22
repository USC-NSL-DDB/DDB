#include <fcntl.h>
#include <math.h>
#include <sched.h>
#include <stdio.h>
#include <sys/mman.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <x86intrin.h>

#include <algorithm>
#include <cstdint>
#include <iostream>
#include <vector>
#define EXPECTED_WORK_MS 1000.0
bool setAffinity(int target_core) {
  cpu_set_t cpuset;
  CPU_ZERO(&cpuset);
  CPU_SET(target_core, &cpuset);

  // Set affinity for the current thread
  int result = pthread_setaffinity_np(pthread_self(), sizeof(cpu_set_t), &cpuset);
  if (result != 0) {
    return false;
  }

  // Verify the affinity was set
  CPU_ZERO(&cpuset);
  result = pthread_getaffinity_np(pthread_self(), sizeof(cpu_set_t), &cpuset);
  if (result != 0) {
    return false;
  }

  if (!CPU_ISSET(target_core, &cpuset)) {
    return false;
  }

  return true;
}
class WorkloadCalibrator {
 private:
  static void dummy_work(uint64_t iterations) {
    volatile double result = 0.0;
    for (uint64_t i = 0; i < iterations; i++) {
      result += (i * 0.5) + (result * 0.1);
    }
  }

  static uint64_t measure_work_time(uint64_t iterations) {
    uint64_t start = __rdtsc();
    dummy_work(iterations);
    uint64_t end = __rdtsc();
    return end - start;
  }

 public:
  static uint64_t calibrate_for_target(double target_ms, double tsc_freq_ghz) {
    uint64_t target_cycles = static_cast<uint64_t>(target_ms * 1e6 * tsc_freq_ghz);

    // Binary search for iteration count
    uint64_t low = 1;
    uint64_t high = 300000000;
    uint64_t best_iters = 0;
    uint64_t best_diff = UINT64_MAX;

    for (int attempts = 0; attempts < 40; attempts++) {
      uint64_t mid = (low + high) / 2;

      // Take multiple samples
      const int SAMPLES = 1;
      std::vector<uint64_t> samples;
      for (int i = 0; i < SAMPLES; i++) {
        samples.push_back(measure_work_time(mid));
      }
      std::sort(samples.begin(), samples.end());
      uint64_t median_cycles = samples[SAMPLES / 2];

      uint64_t diff = (median_cycles > target_cycles) ? (median_cycles - target_cycles) : (target_cycles - median_cycles);

      if (diff < best_diff) {
        best_diff = diff;
        best_iters = mid;
      }

      if (median_cycles < target_cycles) {
        low = mid + 1;
      } else {
        high = mid - 1;
      }
      std::cout << "Curernt diff: " << diff << " Current Iterations: " << mid << " Best diff: " << best_diff << std::endl;
      if (best_diff < 10000) break;
    }

    return best_iters;
  }

  static void perform_calibrated_work(uint64_t iterations) {
    dummy_work(iterations);
  }
};
class BracketedTimestamp {
 public:
  uint64_t tsc_before;
  struct timespec clock_time;
  struct timespec cpu_block_time;
  uint64_t tsc_after;

  static BracketedTimestamp capture() {
    BracketedTimestamp ts;
    ts.tsc_before = __rdtsc();
    clock_gettime(CLOCK_MONOTONIC, &ts.clock_time);
    clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &ts.cpu_block_time);
    ts.tsc_after = __rdtsc();
    return ts;
  }

  // Estimate the most likely TSC value when clock_gettime actually occurred
  uint64_t estimated_tsc() const {
    // Assume clock_gettime occurred at midpoint between TSC readings
    return tsc_before + (tsc_after - tsc_before) / 2;
  }

  // Get the uncertainty window in cycles
  uint64_t uncertainty_cycles() const {
    return tsc_after - tsc_before;
  }
};
class TimeVerifier {
 private:
  std::vector<BracketedTimestamp> checkpoints;
  double tsc_freq_ghz;
  uint64_t calibrated_iterations;

  // Calculate TSC frequency
  static double calibrate_tsc_freq() {
    const int CALIBRATION_MS = 100;
    const int SAMPLES = 10;
    std::vector<double> frequencies;

    for (int i = 0; i < SAMPLES; i++) {
      BracketedTimestamp start = BracketedTimestamp::capture();
      usleep(CALIBRATION_MS * 1000);
      BracketedTimestamp end = BracketedTimestamp::capture();

      uint64_t ns_diff = (end.clock_time.tv_sec - start.clock_time.tv_sec) * 1000000000ULL +
                         (end.clock_time.tv_nsec - start.clock_time.tv_nsec);

      // Use estimated TSC values for better accuracy
      uint64_t tsc_diff = end.estimated_tsc() - start.estimated_tsc();
      frequencies.push_back((double)tsc_diff / ns_diff);

      usleep(10000);  // Short delay between calibration samples
    }

    // Use median frequency to avoid outliers
    std::sort(frequencies.begin(), frequencies.end());
    return frequencies[SAMPLES / 2];
  }

 public:
  TimeVerifier() : tsc_freq_ghz(2.0) {
    printf("System calibration results:\n");
    printf("TSC frequency: %.10f GHz\n", tsc_freq_ghz);

    // Calibrate workload
    // calibrated_iterations = WorkloadCalibrator::calibrate_for_target(EXPECTED_WORK_MS, tsc_freq_ghz);
    calibrated_iterations = 289306640;

    // Verify calibration
    uint64_t verify_start = __rdtsc();
    WorkloadCalibrator::perform_calibrated_work(calibrated_iterations);
    uint64_t verify_end = __rdtsc();
    double achieved_ms = (verify_end - verify_start) / (tsc_freq_ghz * 1e6);

    printf("Workload calibration results:\n");
    printf("Target work time: %.3f ms\n", EXPECTED_WORK_MS);
    printf("Calibrated to: %.3f ms\n", achieved_ms);
    printf("Using %lu iterations\n\n", calibrated_iterations);
  }

  void record_checkpoint() {
    checkpoints.push_back(BracketedTimestamp::capture());
  }

  // Perform calibrated work
  void do_work() {
    WorkloadCalibrator::perform_calibrated_work(calibrated_iterations);
  }

  void analyze_interval(size_t start_idx, size_t end_idx) {
    const BracketedTimestamp &start = checkpoints[start_idx];
    const BracketedTimestamp &end = checkpoints[end_idx];

    // Calculate clock_gettime interval
    double clock_diff_ns =
        (end.clock_time.tv_sec - start.clock_time.tv_sec) * 1e9 +
        (end.clock_time.tv_nsec - start.clock_time.tv_nsec);
    double cpu_block_diff_ns =
        (end.cpu_block_time.tv_sec - start.cpu_block_time.tv_sec) * 1e9 +
        (end.cpu_block_time.tv_nsec - start.cpu_block_time.tv_nsec);

    // Calculate TSC interval using estimated values
    uint64_t tsc_diff = end.estimated_tsc() - start.estimated_tsc();
    double tsc_diff_ns = tsc_diff / tsc_freq_ghz;

    // // Calculate uncertainty windows
    // double start_uncertainty_ns = start.uncertainty_cycles() / tsc_freq_ghz;
    // double end_uncertainty_ns = end.uncertainty_cycles() / tsc_freq_ghz;
    // double total_uncertainty_ns = start_uncertainty_ns + end_uncertainty_ns;

    printf("\nInterval %zu analysis:\n", start_idx);
    printf("Timing measurements:\n");
    printf("  clock_gettime interval: %.3f ms\n", clock_diff_ns / 1e6);
    printf("  CPU clock time: %.3f ms\n", cpu_block_diff_ns / 1e6);
    printf("  RDTSC interval: %.3f ms\n", tsc_diff_ns / 1e6);

    // printf("\nUncertainty analysis:\n");
    // printf("  Start timestamp uncertainty: %.3f μs\n", start_uncertainty_ns / 1000);
    // printf("  End timestamp uncertainty: %.3f μs\n", end_uncertainty_ns / 1000);
    // printf("  Total measurement uncertainty: ±%.3f μs\n", total_uncertainty_ns / 2000);

    // Verify against expected interval
    // double expected_ns = EXPECTED_WORK_MS * 1e6;
    // double clock_deviation = fabs(clock_diff_ns - expected_ns);
    // double tsc_deviation = fabs(tsc_diff_ns - expected_ns);

    // printf("\nDeviation analysis:\n");
    // printf("  Expected interval: %.3f ms\n", expected_ns / 1e6);
    // printf("  clock_gettime deviation: %.3f ms\n", clock_deviation / 1e6);
    // printf("  RDTSC deviation: %.3f ms\n", tsc_deviation / 1e6);

    // if (clock_deviation > (expected_ns * 0.05)) { // 5% tolerance
    //     printf("WARNING: Large clock_gettime deviation from expected interval\n");
    // }

    // printf("\nBounds analysis:\n");
    // printf("  Minimum possible RDTSC interval: %.3f ms\n",
    //        ((end.tsc_before - start.tsc_after) / tsc_freq_ghz) / 1e6);
    printf("  Maximum possible RDTSC interval: %.3f ms\n",
           ((end.tsc_after - start.tsc_before) / tsc_freq_ghz) / 1e6);
  }
};

int main() {
  if (!setAffinity(1)) return 1;
  int cpu_id = sched_getcpu();
  if (cpu_id == -1) {
    perror("sched_getcpu");
  } else {
    std::cout << "Current CPU ID: " << cpu_id << std::endl;
  }
  TimeVerifier verifier;
  // Record initial checkpoint
  // verifier.record_checkpoint();

  // Main test loop with actual work instead of sleep
  // for (int i = 0; i < 10; i++) {
  //   verifier.do_work();
  // }
  printf("Starting workload-calibrated timing verification...\n");
  printf("Will perform calibrated work cycles\n\n");

  // Record initial checkpoint
  verifier.record_checkpoint();

  // Main test loop with actual work instead of sleep
  for (int i = 0; i < 10; i++) {
    verifier.do_work();
  }
  verifier.record_checkpoint();
  for (int i = 0; i < 1; i++) {
    verifier.analyze_interval(i, i + 1);
  }

  return 0;
}