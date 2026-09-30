/* A real monotonic sleep must not disable subsequent realtime adjustments. */
#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double realtime_seconds(void)
{
  struct timespec now;
  assert(clock_gettime(CLOCK_REALTIME, &now) == 0);
  return now.tv_sec + now.tv_nsec / 1e9;
}

int main(void)
{
  char *offset = getenv("FAKETIME");
  assert(offset != NULL && strlen(offset) == strlen("-00000000000000000000.000000000"));

  /* Check successful relative/absolute sleeps and the native error path.
   * Updating the existing environment buffer matches debugger pause accounting.
   */
  for (int mode = 0; mode < 3; ++mode)
  {
    struct timespec request = {0, 1000000};
    if (mode == 1)
    {
      assert(clock_gettime(CLOCK_MONOTONIC, &request) == 0);
    }
    if (mode == 2)
    {
      request.tv_nsec = 1000000000;
    }

    double before = realtime_seconds();
    int result = clock_nanosleep(CLOCK_MONOTONIC,
                                mode == 1 ? TIMER_ABSTIME : 0, &request, NULL);
    assert(result == (mode == 2 ? EINVAL : 0));
    snprintf(offset, strlen(offset) + 1, "-%020d.000000000", mode + 1);
    double delta = realtime_seconds() - before;
    if (delta < -1.1 || delta > -0.8)
    {
      fprintf(stderr, "sleep mode %d lost the realtime offset: delta=%.6f, expected about -1 second\n",
              mode, delta);
      return 1;
    }
  }
  return 0;
}
