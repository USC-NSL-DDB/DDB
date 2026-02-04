#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <semaphore.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/epoll.h>
#endif

static const int kStepMs = 250;
static const int kSlackMs = 120;
static const int kApplyDelayMs = 350;

static int (*libc_clock_gettime_fn)(clockid_t, struct timespec *);
static int (*libc_nanosleep_fn)(const struct timespec *, struct timespec *);

static const char *ts_path(void)
{
  const char *p = getenv("FAKETIME_TIMESTAMP_FILE");
  return (p && *p) ? p : NULL;
}

static int overwrite_file(const char *path, const char *contents)
{
  int fd = open(path, O_WRONLY | O_TRUNC | O_CREAT | O_CLOEXEC, 0644);
  if (fd < 0) return -1;
  size_t len = strlen(contents);
  ssize_t wr = write(fd, contents, len);
  int saved = errno;
  close(fd);
  errno = saved;
  return (wr == (ssize_t)len) ? 0 : -1;
}

static void ms_to_timespec(int ms, struct timespec *ts)
{
  ts->tv_sec = ms / 1000;
  ts->tv_nsec = (long)(ms % 1000) * 1000000L;
}

static void timespec_add(const struct timespec *a, const struct timespec *b, struct timespec *out)
{
  out->tv_sec = a->tv_sec + b->tv_sec;
  out->tv_nsec = a->tv_nsec + b->tv_nsec;
  if (out->tv_nsec >= 1000000000L)
  {
    out->tv_sec += 1;
    out->tv_nsec -= 1000000000L;
  }
}

static long elapsed_ms(const struct timespec *start, const struct timespec *end)
{
  long sec = (long)(end->tv_sec - start->tv_sec);
  long nsec = end->tv_nsec - start->tv_nsec;
  if (nsec < 0)
  {
    sec -= 1;
    nsec += 1000000000L;
  }
  return sec * 1000L + nsec / 1000000L;
}

static int init_libc_syms(void)
{
  void *h = dlopen("libc.so.6", RTLD_NOW);
  if (!h)
  {
    h = dlopen("libc.so", RTLD_NOW);
  }
  if (!h) return -1;
  libc_clock_gettime_fn = (int (*)(clockid_t, struct timespec *))dlsym(h, "clock_gettime");
  libc_nanosleep_fn = (int (*)(const struct timespec *, struct timespec *))dlsym(h, "nanosleep");
  if (!libc_clock_gettime_fn || !libc_nanosleep_fn) return -1;
  return 0;
}

typedef int (*child_fn_t)(void);

static int run_case(const char *name, int req_ms, child_fn_t fn)
{
  const char *path = ts_path();
  if (!path)
  {
    fprintf(stderr, "[%s] missing FAKETIME_TIMESTAMP_FILE\n", name);
    return 1;
  }

  if (overwrite_file(path, "+0\n") != 0)
  {
    fprintf(stderr, "[%s] cannot write timestamp file: %s\n", name, strerror(errno));
    return 1;
  }

  pid_t pid = fork();
  if (pid < 0)
  {
    fprintf(stderr, "[%s] fork failed: %s\n", name, strerror(errno));
    return 1;
  }

  if (pid == 0)
  {
    struct timespec t0, t1;
    if (libc_clock_gettime_fn(CLOCK_MONOTONIC, &t0) != 0) _exit(3);

    int rc = fn();
    if (rc == 77) _exit(77); /* skip */
    if (rc != 0) _exit(4);

    if (libc_clock_gettime_fn(CLOCK_MONOTONIC, &t1) != 0) _exit(5);
    long ms = elapsed_ms(&t0, &t1);

    long expected_min = (long)req_ms + (long)kStepMs - (long)kSlackMs;
    if (ms < expected_min)
    {
      fprintf(stderr, "[%s] FAIL elapsed=%ldms expected>=%ldms\n", name, ms, expected_min);
      _exit(6);
    }

    _exit(0);
  }

  struct timespec delay;
  ms_to_timespec(kApplyDelayMs, &delay);
  (void)libc_nanosleep_fn(&delay, NULL);

  char step_str[64];
  snprintf(step_str, sizeof(step_str), "-%.9g\n", (double)kStepMs / 1000.0);
  if (overwrite_file(path, step_str) != 0)
  {
    fprintf(stderr, "[%s] cannot step timestamp file: %s\n", name, strerror(errno));
    return 1;
  }

  int status = 0;
  if (waitpid(pid, &status, 0) < 0)
  {
    fprintf(stderr, "[%s] waitpid failed: %s\n", name, strerror(errno));
    return 1;
  }

  if (WIFEXITED(status))
  {
    int code = WEXITSTATUS(status);
    if (code == 0)
    {
      printf("[%s] PASS\n", name);
      return 0;
    }
    if (code == 77)
    {
      printf("[%s] SKIP\n", name);
      return 0;
    }
    printf("[%s] FAIL (exit=%d)\n", name, code);
    return 1;
  }

  if (WIFSIGNALED(status))
  {
    printf("[%s] FAIL (signal=%d)\n", name, WTERMSIG(status));
    return 1;
  }

  printf("[%s] FAIL (unknown status)\n", name);
  return 1;
}

static int child_nanosleep_rel(void)
{
  struct timespec req;
  ms_to_timespec(500, &req);
  if (nanosleep(&req, NULL) != 0) return 1;
  return 0;
}

static int child_clock_nanosleep_rel_realtime(void)
{
  struct timespec req;
  ms_to_timespec(500, &req);
  int rc = clock_nanosleep(CLOCK_REALTIME, 0, &req, NULL);
  return (rc == 0) ? 0 : 1;
}

static int child_clock_nanosleep_abs_realtime(void)
{
  struct timespec now, req, abs;
  if (clock_gettime(CLOCK_REALTIME, &now) != 0) return 1;
  ms_to_timespec(500, &req);
  timespec_add(&now, &req, &abs);
  int rc = clock_nanosleep(CLOCK_REALTIME, TIMER_ABSTIME, &abs, NULL);
  return (rc == 0) ? 0 : 1;
}

static int child_usleep_rel(void)
{
  if (usleep(500 * 1000) != 0) return 1;
  return 0;
}

static int child_sleep_1s(void)
{
  unsigned int r = sleep(1);
  return (r == 0) ? 0 : 1;
}

static int child_poll_timeout(void)
{
  int rc = poll(NULL, 0, 500);
  return (rc == 0) ? 0 : 1;
}

static int child_ppoll_timeout(void)
{
  struct timespec ts;
  ms_to_timespec(500, &ts);
  int rc = ppoll(NULL, 0, &ts, NULL);
  return (rc == 0) ? 0 : 1;
}

static int child_select_timeout(void)
{
  struct timeval tv;
  tv.tv_sec = 0;
  tv.tv_usec = 500 * 1000;
  int rc = select(0, NULL, NULL, NULL, &tv);
  return (rc == 0) ? 0 : 1;
}

static int child_pselect_timeout(void)
{
  struct timespec ts;
  ms_to_timespec(500, &ts);
  int rc = pselect(0, NULL, NULL, NULL, &ts, NULL);
  return (rc == 0) ? 0 : 1;
}

#ifdef __linux__
static int child_epoll_wait_timeout(void)
{
  int epfd = epoll_create1(0);
  if (epfd < 0) return 1;
  struct epoll_event ev;
  int rc = epoll_wait(epfd, &ev, 1, 500);
  close(epfd);
  return (rc == 0) ? 0 : 1;
}

static int child_epoll_pwait_timeout(void)
{
  int epfd = epoll_create1(0);
  if (epfd < 0) return 1;
  struct epoll_event ev;
  int rc = epoll_pwait(epfd, &ev, 1, 500, NULL);
  close(epfd);
  return (rc == 0) ? 0 : 1;
}
#endif

static int child_sem_timedwait_timeout(void)
{
  sem_t sem;
  if (sem_init(&sem, 0, 0) != 0) return 1;

  struct timespec now, req, abs;
  if (clock_gettime(CLOCK_REALTIME, &now) != 0) return 1;
  ms_to_timespec(500, &req);
  timespec_add(&now, &req, &abs);

  errno = 0;
  int rc = sem_timedwait(&sem, &abs);
  int saved = errno;
  sem_destroy(&sem);
  if (rc != -1) return 1;
  return (saved == ETIMEDOUT) ? 0 : 1;
}

static int child_sem_clockwait_timeout_realtime(void)
{
  void *h = dlopen("libc.so.6", RTLD_NOW);
  if (!h) h = dlopen("libc.so", RTLD_NOW);
  if (!h) return 77;
  void *sym = dlsym(h, "sem_clockwait");
  if (!sym) return 77;

  sem_t sem;
  if (sem_init(&sem, 0, 0) != 0) return 1;

  struct timespec now, req, abs;
  if (clock_gettime(CLOCK_REALTIME, &now) != 0) return 1;
  ms_to_timespec(500, &req);
  timespec_add(&now, &req, &abs);

  errno = 0;
  int rc = sem_clockwait(&sem, CLOCK_REALTIME, &abs);
  int saved = errno;
  sem_destroy(&sem);
  if (rc != -1) return 1;
  return (saved == ETIMEDOUT) ? 0 : 1;
}

static int child_pthread_cond_timedwait_timeout_realtime(void)
{
  pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
  pthread_cond_t c = PTHREAD_COND_INITIALIZER;

  struct timespec now, req, abs;
  if (clock_gettime(CLOCK_REALTIME, &now) != 0) return 1;
  ms_to_timespec(500, &req);
  timespec_add(&now, &req, &abs);

  pthread_mutex_lock(&m);
  int rc = pthread_cond_timedwait(&c, &m, &abs);
  pthread_mutex_unlock(&m);

  return (rc == ETIMEDOUT) ? 0 : 1;
}

static int child_pthread_cond_timedwait_timeout_monotonic(void)
{
  pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
  pthread_cond_t c;
  pthread_condattr_t a;
  if (pthread_condattr_init(&a) != 0) return 1;
  if (pthread_condattr_setclock(&a, CLOCK_MONOTONIC) != 0) return 77;
  if (pthread_cond_init(&c, &a) != 0) return 1;
  pthread_condattr_destroy(&a);

  struct timespec now, req, abs;
  if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 1;
  ms_to_timespec(500, &req);
  timespec_add(&now, &req, &abs);

  pthread_mutex_lock(&m);
  int rc = pthread_cond_timedwait(&c, &m, &abs);
  pthread_mutex_unlock(&m);

  pthread_cond_destroy(&c);
  return (rc == ETIMEDOUT) ? 0 : 1;
}

int main(void)
{
  if (init_libc_syms() != 0)
  {
    fprintf(stderr, "failed to resolve libc symbols\n");
    return 2;
  }

  int failures = 0;

  failures += run_case("nanosleep(rel)", 500, child_nanosleep_rel);
  failures += run_case("clock_nanosleep(rel,realtime)", 500, child_clock_nanosleep_rel_realtime);
  failures += run_case("clock_nanosleep(abs,realtime)", 500, child_clock_nanosleep_abs_realtime);
  failures += run_case("usleep(rel)", 500, child_usleep_rel);
  failures += run_case("sleep(1s)", 1000, child_sleep_1s);
  failures += run_case("poll(timeout)", 500, child_poll_timeout);
  failures += run_case("ppoll(timeout)", 500, child_ppoll_timeout);
  failures += run_case("select(timeout)", 500, child_select_timeout);
  failures += run_case("pselect(timeout)", 500, child_pselect_timeout);
#ifdef __linux__
  failures += run_case("epoll_wait(timeout)", 500, child_epoll_wait_timeout);
  failures += run_case("epoll_pwait(timeout)", 500, child_epoll_pwait_timeout);
#endif
  failures += run_case("sem_timedwait(abs)", 500, child_sem_timedwait_timeout);
  failures += run_case("sem_clockwait(abs,realtime)", 500, child_sem_clockwait_timeout_realtime);
  failures += run_case("pthread_cond_timedwait(abs,realtime)", 500, child_pthread_cond_timedwait_timeout_realtime);
  failures += run_case("pthread_cond_timedwait(abs,monotonic)", 500, child_pthread_cond_timedwait_timeout_monotonic);

  if (failures != 0)
  {
    fprintf(stderr, "%d test(s) failed\n", failures);
  }
  return failures ? 1 : 0;
}

