#define _GNU_SOURCE
#include <stdio.h>
#include "ddb/common.h"
#include "ddb/logger.h"

extern ddb_shmseg *ddb_shared;

void dump_shared_memory() {
  printf("Dumping ddb_shared state:\n");
  printf("\tnthreads: %d\n", ddb_shared->ddb_nthread);
  printf("\tmax thrd idx: %d\n", ddb_shared->ddb_max_idx);
  printf("------ Thread infos at: %p\n", (void*)ddb_shared->ddb_thread_infos);
  pthread_spin_lock(&(ddb_shared->ddb_tlock));
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
        ddb_shared->ddb_lowners.lowner_entries[i].owner_tid,
        (void*)ddb_shared->ddb_lowners.lowner_entries[i].lid);
    }
  }
  printf("------ Lock owners ends\n");
  pthread_spin_unlock(&(ddb_shared->ddb_tlock));
}
