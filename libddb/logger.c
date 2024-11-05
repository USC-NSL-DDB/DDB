#define _GNU_SOURCE
#include "ddb/common.h"
#include "ddb/logger.h"

#define LDB_EVENT_OUTPUT "ddb.data"

extern ddb_shmseg *ddb_shared;
extern bool running;
bool reset;

void *logger_main(void *arg) {
  FILE *ddb_fout = fopen(LDB_EVENT_OUTPUT, "wb");
  char cmd_map_buf[128];
  // ldb_event_buffer_t *ebuf;
  ddb_wait_buffer_t *wbuf;
  int head;
  int len;

  printf("logger thread starts\n");

  cpu_set_t cpuset;

  CPU_ZERO(&cpuset);
  CPU_SET(1, &cpuset);
  pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);

  // store maps
  pid_t pid_self = syscall(SYS_getpid);
  sprintf(cmd_map_buf, "cat /proc/%d/maps > maps.data", pid_self);
  // does map information changes over time?
  system(cmd_map_buf);

  while (running) {
    if (unlikely(reset)) {
      fclose(ddb_fout);
      ddb_fout = fopen(LDB_EVENT_OUTPUT, "wb");
      reset = false;
    }

    for (int tidx = 0; tidx < ddb_shared->ddb_max_idx; ++tidx) {
      // Skip if event buffer is not valid
      if (ddb_shared->ddb_thread_infos[tidx].wbuf == NULL) {
        continue;
      }

      wbuf = ddb_shared->ddb_thread_infos[tidx].wbuf;
      head = wbuf->head;
      barrier();
      len = wbuf->tail - head;

      if (len <= 0) {
        continue;
      }
      int end = LDB_EVENT_BUF_SIZE - (head % LDB_EVENT_BUF_SIZE);
      if (len > end) len = end;

      fwrite(&wbuf->wait_entries[head % LDB_EVENT_BUF_SIZE], sizeof(ddb_wait_entry_t), len, ddb_fout);
      fflush(ddb_fout);

      wbuf->head = head + len;
    }// for
  }// while (running)

  fclose(ddb_fout);

  printf("logger thread exiting...\n");
  return NULL;
}

void logger_reset() {
  reset = true;
}
