#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <errno.h>
#include <sys/types.h>

#include "ddb/common.h"

// static pthread_t monitor_th;
// static pthread_t logger_th;

extern ddb_shmseg *ddb_shared;
bool running;

// extern void *monitor_main(void *arg);
// extern void *logger_main(void *arg);

// This is the main function instrumented
void __ddbInit(void) {
  // initialize stack
  char *rbp = get_rbp(); // this is rbp of __ddbInit()
  rbp = (char *)(*((uint64_t *)rbp)); // this is rbp of main()

  // // *((uint64_t *)(rbp + 16)) = 0;
  // // *((uint64_t *)(rbp + 8)) = (uint64_t)LDB_CANARY << 32;
  // // *((uint64_t *)rbp) = 0;

  key_t key = ftok("/tmp", 'R'); // Generate unique key
  if (key == -1) {
      perror("ftok failed");
      exit(1);
  }

  // attach shared memory
  int shmid = shmget(key, sizeof(ddb_shmseg), 0666 | IPC_CREAT);
  if (shmid == -1) {
      char buf[256];
      snprintf(buf, sizeof(buf), 
              "shmget failed: errno=%d (%s)\n", 
              errno, strerror(errno));
      write(STDERR_FILENO, buf, strlen(buf));
    }
  ddb_shared = shmat(shmid, NULL, 0);
  memset(ddb_shared, 0, sizeof(ddb_shmseg));
  printf("shmid: %d\n", shmid); 
  printf("ddb_shared: %p\n", (void *)ddb_shared);

  
	// initialize thread info
  printf("mid malloc\n");
  ddb_shared->ddb_thread_infos = (ddb_thread_info_t *) malloc(sizeof(ddb_thread_info_t) * LDB_MAX_NTHREAD);
  printf("after malloc\n");
  memset(ddb_shared->ddb_thread_infos, 0, sizeof(ddb_thread_info_t) * LDB_MAX_NTHREAD);

  // initialize lock owner info
  ddb_shared->ddb_lowners.lowner_entries = (ddb_lowner_entry_t*) malloc(sizeof(ddb_lowner_entry_t) * DDB_MAX_NLOCK);
  memset(ddb_shared->ddb_lowners.lowner_entries, 0, sizeof(ddb_lowner_entry_t) * DDB_MAX_NLOCK);

  // allocate & initialize event buffer
  ddb_wait_buffer_t *wbuf = (ddb_wait_buffer_t *) malloc(sizeof(ddb_wait_buffer_t));
  memset(wbuf, 0, sizeof(ddb_wait_buffer_t));
  wbuf->wait_entries = (ddb_wait_entry_t *) malloc(sizeof(ddb_wait_entry_t) * DDB_MAX_NWAIT);

  // struct timespec now;
  // clock_gettime(CLOCK_MONOTONIC_RAW, &now);

  // initialize main thread's info
  ddb_shared->ddb_thread_infos[0].id = syscall(SYS_gettid);
  ddb_shared->ddb_thread_infos[0].fsbase = (char **)(rdfsbase());
  ddb_shared->ddb_thread_infos[0].stackbase = rbp;
  ddb_shared->ddb_thread_infos[0].wbuf = wbuf;

  ddb_shared->ddb_nthread = 1;
  ddb_shared->ddb_max_idx = 1;

  // // register_thread_info(0);

  pthread_spin_init(&ddb_shared->ddb_tlock, PTHREAD_PROCESS_SHARED);

  running = true;

  // Launch logger thread
  // pthread_create(&logger_th, NULL, &logger_main, NULL);
}

void __ddbExit(void) {
  void *ret;

  // Join monitor and destroy spin lock?
  printf("Main app is exiting...\n");
  running = false;

  // pthread_join(monitor_th, &ret);
  // pthread_join(logger_th, &ret);

  printf("Dumping ddb_shared state:\n");
  printf("Number of threads: %d\n", ddb_shared->ddb_nthread);
  printf("Max thread index: %d\n", ddb_shared->ddb_max_idx);
  printf("Thread infos at: %p\n", (void*)ddb_shared->ddb_thread_infos);
  printf("Lock owners at: %p\n", (void*)ddb_shared->ddb_lowners.lowner_entries);
  for (int i = 0; i < ddb_shared->ddb_nthread; i++) {
    printf("Thread %d: id=%d, fsbase=%p, stackbase=%p\n", 
         i,
         ddb_shared->ddb_thread_infos[i].id,
         (void*)ddb_shared->ddb_thread_infos[i].fsbase,
         (void*)ddb_shared->ddb_thread_infos[i].stackbase);
  }
  for (int i = 0; i < ddb_shared->ddb_lowners.max_n; i++) {
    if (ddb_shared->ddb_lowners.lowner_entries[i].valid) {
      printf("Lock %d: owner=%d, lock_addr=%p\n",
        i,
        ddb_shared->ddb_lowners.lowner_entries[i].tid,
        (void*)ddb_shared->ddb_lowners.lowner_entries[i].lptr);
    }
  }

  for (int i = 0; i < ddb_shared->ddb_max_idx; i++) {
    if (ddb_shared->ddb_thread_infos[i].wbuf) {
      printf("Thread %d wait buffer entries:\n", i);
      ddb_wait_buffer_t *wbuf = ddb_shared->ddb_thread_infos[i].wbuf;
      for (int j = 0; j < wbuf->max_n; j++) {
        if (wbuf->wait_entries[j].valid) {
          printf("  Entry %d: id=%p, wait_type=%d\n",
            j,
            (void*)wbuf->wait_entries[j].identifier,
            wbuf->wait_entries[j].type);
        }
      }
    }
  }

  free(ddb_shared->ddb_thread_infos[0].wbuf->wait_entries);
  free(ddb_shared->ddb_thread_infos[0].wbuf);
  free(ddb_shared->ddb_thread_infos);

  shmdt(ddb_shared);
}

void sig_hdlr(int signum) {
  printf("Received signal %d, exiting...\n", signum);
  __ddbExit();
}

__attribute__((constructor)) void init() {
    atexit(__ddbExit);
    signal(SIGTERM, sig_hdlr);
    signal(SIGINT, sig_hdlr);

    printf("Hooked!\n");
    __ddbInit();
}
