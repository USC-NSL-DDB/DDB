#include <stdio.h>
#define _GNU_SOURCE
#include "ddb/common.h"
#include "ddb/logger.h"

// #define LDB_EVENT_OUTPUT "ddb.data"

extern ddb_shmseg *ddb_shared;
extern bool running;
bool reset;

void dump_shared_memory() {
// FILE *fp = fopen("shared_memory_dump", "wb");
  // fwrite(ddb_shared, sizeof(ddb_shmseg), 1, fp);
  // fclose(fp);
  printf("Dumping ddb_shared state:\n");
  printf("\tnthreads: %d\n", ddb_shared->ddb_nthread);
  printf("\tmax thrd idx: %d\n", ddb_shared->ddb_max_idx);
  printf("------ Thread infos at: %p\n", (void*)ddb_shared->ddb_thread_infos);
  for (int i = 0; i < ddb_shared->ddb_max_idx; i++) {
    if (!ddb_shared->ddb_thread_infos[i].valid) { 
      continue; 
    }
    printf("\tThread %d: valid=%d, id=%d, fsbase=%p, stackbase=%p\n", 
         i,
          ddb_shared->ddb_thread_infos[i].valid,
         ddb_shared->ddb_thread_infos[i].id,
         (void*)ddb_shared->ddb_thread_infos[i].fsbase,
         (void*)ddb_shared->ddb_thread_infos[i].stackbase);
    // for (int i = 0; i < ddb_shared->ddb_max_idx; i++) {
      if (ddb_shared->ddb_thread_infos[i].wbuf) {
        printf("\tThread %d wait buffer entries:\n", i);
        ddb_wait_buffer_t *wbuf = ddb_shared->ddb_thread_infos[i].wbuf;
        for (int j = 0; j < wbuf->max_n; j++) {
          if (wbuf->wait_entries[j].valid) {
            printf("\t  Entry %d: id=%p, wait_type=%d\n",
              j,
              (void*)wbuf->wait_entries[j].identifier,
              wbuf->wait_entries[j].type);
          }
        }
      }
    // }
  }
  printf("------ Thread infos ends\n");

  printf("------ Lock owners at: %p\n", (void*)ddb_shared->ddb_lowners.lowner_entries);
  for (int i = 0; i < ddb_shared->ddb_lowners.max_n; i++) {
    if (ddb_shared->ddb_lowners.lowner_entries[i].valid) {
      printf("\tLock %d: owner=%d, lock_addr=%p\n",
        i,
        ddb_shared->ddb_lowners.lowner_entries[i].tid,
        (void*)ddb_shared->ddb_lowners.lowner_entries[i].lptr);
    }
  }
  printf("------ Lock owners ends\n");

}

// void *logger_main(void *arg) {
//   FILE *ddb_fout = fopen(LDB_EVENT_OUTPUT, "wb");
//   char cmd_map_buf[128];
//   // ldb_event_buffer_t *ebuf;
//   ddb_wait_buffer_t *wbuf;
//   int head;
//   int len;

//   printf("logger thread starts\n");

//   cpu_set_t cpuset;

//   CPU_ZERO(&cpuset);
//   CPU_SET(1, &cpuset);
//   pthread_setaffinity_np(pthread_self(), sizeof(cpuset), &cpuset);

//   // store maps
//   pid_t pid_self = syscall(SYS_getpid);
//   sprintf(cmd_map_buf, "cat /proc/%d/maps > maps.data", pid_self);
//   // does map information changes over time?
//   system(cmd_map_buf);

//   while (running) {
//     if (unlikely(reset)) {
//       fclose(ddb_fout);
//       ddb_fout = fopen(LDB_EVENT_OUTPUT, "wb");
//       reset = false;
//     }

//     for (int tidx = 0; tidx < ddb_shared->ddb_max_idx; ++tidx) {
//       // Skip if event buffer is not valid
//       if (ddb_shared->ddb_thread_infos[tidx].wbuf == NULL) {
//         continue;
//       }

//       wbuf = ddb_shared->ddb_thread_infos[tidx].wbuf;
//       // head = wbuf->head;
//       // barrier();
//       // len = wbuf->tail - head;

//       // if (len <= 0) {
//       //   continue;
//       // }
//       // int end = LDB_EVENT_BUF_SIZE - (head % LDB_EVENT_BUF_SIZE);
//       // if (len > end) len = end;

//       fwrite(&wbuf->wait_entries[0], sizeof(ddb_wait_entry_t), DDB_MAX_NWAIT, ddb_fout);
//       fflush(ddb_fout);

//       // wbuf->head = head + len;
//     }// for
//   }// while (running)

//   fclose(ddb_fout);

//   printf("logger thread exiting...\n");
//   return NULL;
// }

// void logger_reset() {
//   reset = true;
// }
