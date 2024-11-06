#include <dlfcn.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <pthread.h>
#include <sys/types.h>

#include "ddb/common.h"
#include "ddb/logger.h"

ddb_shmseg *ddb_shared = NULL;
pthread_key_t ddb_tls_key;
bool running;
bool cleaned;

static void cleanup_tls(void* data) {
    free(data);
}

// This is the main function instrumented
void __ddbInit(void) {
  // initialize stack
  char *rbp = get_rbp(); // this is rbp of __ddbInit()
  rbp = (char *)(*((uint64_t *)rbp)); // this is rbp of main()

  ddb_shared = (ddb_shmseg *) malloc(sizeof(ddb_shmseg));
  memset(ddb_shared, 0, sizeof(ddb_shmseg));
  
	// initialize thread info
  ddb_shared->ddb_thread_infos = (ddb_thread_info_t *) malloc(sizeof(ddb_thread_info_t) * LDB_MAX_NTHREAD);
  memset(ddb_shared->ddb_thread_infos, 0, sizeof(ddb_thread_info_t) * LDB_MAX_NTHREAD);

  // initialize lock owner info
  ddb_shared->ddb_lowners.lowner_entries = (ddb_lowner_entry_t*) malloc(sizeof(ddb_lowner_entry_t) * DDB_MAX_NLOCK);
  memset(ddb_shared->ddb_lowners.lowner_entries, 0, sizeof(ddb_lowner_entry_t) * DDB_MAX_NLOCK);

  // allocate & initialize event buffer
  ddb_wait_buffer_t *wbuf = (ddb_wait_buffer_t *) malloc(sizeof(ddb_wait_buffer_t));
  memset(wbuf, 0, sizeof(ddb_wait_buffer_t));
  wbuf->wait_entries = (ddb_wait_entry_t *) malloc(sizeof(ddb_wait_entry_t) * DDB_MAX_NWAIT);

  cleaned = false;

  // initialize main thread's info
  ddb_shared->ddb_thread_infos[0].valid = true;
  ddb_shared->ddb_thread_infos[0].id = syscall(SYS_gettid);
  ddb_shared->ddb_thread_infos[0].fsbase = (char **)(rdfsbase());
  ddb_shared->ddb_thread_infos[0].stackbase = rbp;
  ddb_shared->ddb_thread_infos[0].wbuf = wbuf;

  ddb_shared->ddb_nthread = 1;
  ddb_shared->ddb_max_idx = 1;

  // printf("thread id: %d\n", ddb_shared->ddb_thread_infos[0].id);

  // Initialize the pthread key
  if (pthread_key_create(&ddb_tls_key, cleanup_tls) != 0) {
    perror("tls key creation failed");
    exit(1);
  }

  register_thread_info(0);

  pthread_spin_init(&ddb_shared->ddb_tlock, PTHREAD_PROCESS_SHARED);

  running = true;
}

void __ddbExit(void) {
  printf("Main app is exiting...\n");
  running = false;

  // dump_shared_memory();

  if (ddb_shared && !cleaned) {
    free(ddb_shared->ddb_thread_infos[0].wbuf->wait_entries);
    free(ddb_shared->ddb_thread_infos[0].wbuf);
    free(ddb_shared->ddb_thread_infos);
    free(ddb_shared);
    cleaned = true;
  }

  // clean up TLS
  pthread_key_delete(ddb_tls_key);
}

void sig_hdlr(int signum) {
  printf("Received signal %d, exiting...\n", signum);
  __ddbExit();
  // Restore default handler
  signal(signum, SIG_DFL);
  // Rethrow the signal
  raise(signum);
}

__attribute__((constructor)) void init() {
    atexit(__ddbExit);
    signal(SIGTERM, sig_hdlr);
    signal(SIGINT, sig_hdlr);

    printf("Hooked!\n");
    __ddbInit();
}
