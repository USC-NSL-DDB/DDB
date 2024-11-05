#include "ddb/common.h"
#include <stdint.h>

extern ddb_shmseg *ddb_shared;

void mark_wait(
    ddb_wait_buffer_t *wbuf, uint8_t wait_type, 
    struct timespec ts, uint64_t identifier
) {

  if (unlikely(!wbuf || !wbuf->wait_entries))
    return;

  int tail = wbuf->tail;
  barrier();
  
  if (tail >= wbuf->head + LDB_EVENT_BUF_SIZE) {
    fprintf(stderr, "[%ld] WARNING: wait buffer full: wait resource ignored\n", syscall(SYS_gettid));
    return;
  }

  ddb_wait_entry_t *e = &wbuf->wait_entries[tail % LDB_EVENT_BUF_SIZE];

  e->type = wait_type;
  e->identifier = identifier;

  barrier();

  wbuf->tail = tail + 1;
}

void event_record(
    ddb_wait_buffer_t *wbuf, uint8_t wait_type, 
    struct timespec ts, uint64_t identifier
) {

  if (unlikely(!wbuf || !wbuf->wait_entries))
    return;

  int tail = wbuf->tail;
  barrier();
  
  if (tail >= wbuf->head + LDB_EVENT_BUF_SIZE) {
    fprintf(stderr, "[%ld] WARNING: wait buffer full: wait resource ignored\n", syscall(SYS_gettid));
    return;
  }

  ddb_wait_entry_t *e = &wbuf->wait_entries[tail % LDB_EVENT_BUF_SIZE];

  e->type = wait_type;
  e->identifier = identifier;

  barrier();

  wbuf->tail = tail + 1;
}

inline void event_record_now(uint8_t wait_type) {
  // attach shared memory
  if (unlikely(!ddb_shared)) {
    ddb_shared = attach_shared_memory();
  }

  struct timespec now;
  int tinfo_idx = get_thread_info_idx();
  ddb_thread_info_t *tinfo = &ddb_shared->ddb_thread_infos[tinfo_idx];

  clock_gettime(CLOCK_MONOTONIC_RAW, &now);

  event_record(tinfo->wbuf, wait_type, now, tinfo->id);
}
