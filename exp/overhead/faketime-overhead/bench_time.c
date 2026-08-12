/*
 * bench_time.c -- per-call latency of the time APIs libfaketime interposes.
 *
 * Measures gettimeofday(2), clock_gettime(CLOCK_REALTIME) and
 * clock_gettime(CLOCK_MONOTONIC), each averaged over a tight loop of many
 * calls, timed with a calibrated rdtsc. These calls normally complete in the
 * vDSO in ~20ns, far below clock_gettime's own resolution overhead -- hence
 * rdtsc, and hence loop-averaging instead of timing single calls.
 *
 * The harness runs this binary once per interposition config (with and
 * without libfaketime preloaded); LD_PRELOAD is process-wide, so the two
 * configs cannot share a process.
 *
 * Calibration note: under libfaketime the calibration clock is intercepted
 * too, but interception with a *constant* offset shifts timestamps without
 * changing durations, so the measured cycles/ns is still correct. (A
 * rate-warping FAKETIME spec like "x2" would break this; the harness only
 * uses "+0".)
 *
 * Output, one line per API, parse-friendly:
 *   RESULT api=<name> min_ns=<..> median_ns=<..> mean_ns=<..> rounds=<R> calls=<N>
 * plus one INTERPOSED yes|no line (read from /proc/self/maps) so the harness
 * can verify the config it asked for is the config that ran.
 */

#define _GNU_SOURCE
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <x86intrin.h>

#define ROUNDS 15
#define CALLS_PER_ROUND 100000
#define WARMUP_CALLS 20000

static inline uint64_t rdtsc_begin(void) {
  unsigned aux;
  _mm_lfence();
  uint64_t t = __rdtscp(&aux);
  _mm_lfence();
  return t;
}

static inline uint64_t rdtsc_end(void) {
  unsigned aux;
  uint64_t t = __rdtscp(&aux); /* rdtscp waits for prior instructions */
  _mm_lfence();
  return t;
}

/* Cycles per nanosecond, calibrated against a ~200ms CLOCK_MONOTONIC_RAW
 * interval. Done twice; the two estimates must agree to 0.5% or we abort
 * rather than report latencies off by a calibration error. */
static double calibrate_once(void) {
  struct timespec a, b;
  clock_gettime(CLOCK_MONOTONIC_RAW, &a);
  uint64_t c0 = rdtsc_begin();
  do {
    clock_gettime(CLOCK_MONOTONIC_RAW, &b);
  } while ((b.tv_sec - a.tv_sec) * 1000000000LL + (b.tv_nsec - a.tv_nsec) <
           200 * 1000000LL);
  uint64_t c1 = rdtsc_end();
  double ns = (double)((b.tv_sec - a.tv_sec) * 1000000000LL +
                       (b.tv_nsec - a.tv_nsec));
  return (double)(c1 - c0) / ns;
}

static double calibrate(void) {
  double f1 = calibrate_once();
  double f2 = calibrate_once();
  double rel = (f1 > f2 ? f1 - f2 : f2 - f1) / f1;
  if (rel > 0.005) {
    fprintf(stderr, "calibration unstable: %.6f vs %.6f cycles/ns\n", f1, f2);
    exit(1);
  }
  fprintf(stderr, "# tsc calibration: %.6f cycles/ns\n", (f1 + f2) / 2);
  return (f1 + f2) / 2;
}

/* Sink defeats dead-code elimination of the measured calls. */
static volatile int64_t sink;

typedef void (*bench_fn)(long n);

static void bench_gettimeofday(long n) {
  struct timeval tv;
  for (long i = 0; i < n; i++) {
    gettimeofday(&tv, NULL);
    sink += tv.tv_usec;
  }
}

static void bench_clock_realtime(long n) {
  struct timespec ts;
  for (long i = 0; i < n; i++) {
    clock_gettime(CLOCK_REALTIME, &ts);
    sink += ts.tv_nsec;
  }
}

static void bench_clock_monotonic(long n) {
  struct timespec ts;
  for (long i = 0; i < n; i++) {
    clock_gettime(CLOCK_MONOTONIC, &ts);
    sink += ts.tv_nsec;
  }
}

static int cmp_double(const void *a, const void *b) {
  double x = *(const double *)a, y = *(const double *)b;
  return (x > y) - (x < y);
}

static void run_bench(const char *name, bench_fn fn, double cyc_per_ns) {
  double per_call[ROUNDS];

  fn(WARMUP_CALLS);

  for (int r = 0; r < ROUNDS; r++) {
    uint64_t c0 = rdtsc_begin();
    fn(CALLS_PER_ROUND);
    uint64_t c1 = rdtsc_end();
    per_call[r] = (double)(c1 - c0) / cyc_per_ns / CALLS_PER_ROUND;
  }

  double mean = 0;
  for (int r = 0; r < ROUNDS; r++) mean += per_call[r];
  mean /= ROUNDS;
  qsort(per_call, ROUNDS, sizeof(double), cmp_double);

  /* min = least interference; median = typical. Report both. */
  printf("RESULT api=%s min_ns=%.2f median_ns=%.2f mean_ns=%.2f rounds=%d calls=%d\n",
         name, per_call[0], per_call[ROUNDS / 2], mean, ROUNDS,
         CALLS_PER_ROUND);
}

/* Is libfaketime actually loaded into this process? LD_PRELOAD can fail
 * silently (bad path, ld.so policy); a "faketime" run without interposition
 * would masquerade as zero overhead, so the harness checks this line. */
static void report_interposition(void) {
  FILE *f = fopen("/proc/self/maps", "r");
  char line[512];
  int found = 0;
  if (f) {
    while (fgets(line, sizeof(line), f))
      if (strstr(line, "libfaketime")) { found = 1; break; }
    fclose(f);
  }
  printf("INTERPOSED %s\n", found ? "yes" : "no");
}

int main(int argc, char **argv) {
  int cpu = (argc > 1) ? atoi(argv[1]) : 2;

  cpu_set_t set;
  CPU_ZERO(&set);
  CPU_SET(cpu, &set);
  if (sched_setaffinity(0, sizeof(set), &set) != 0) {
    perror("sched_setaffinity");
    return 1;
  }

  report_interposition();
  double cyc_per_ns = calibrate();

  run_bench("gettimeofday", bench_gettimeofday, cyc_per_ns);
  run_bench("clock_gettime_realtime", bench_clock_realtime, cyc_per_ns);
  run_bench("clock_gettime_monotonic", bench_clock_monotonic, cyc_per_ns);

  /* keep the sink observable */
  fprintf(stderr, "# sink=%ld\n", (long)sink);
  return 0;
}
