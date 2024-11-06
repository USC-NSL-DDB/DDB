#pragma once

#include "ddb/lock.h"
#include <inttypes.h>
#include <sched.h>
#include <stdint.h>
#include <unistd.h>
#include <pthread.h>
#include <bits/pthreadtypes.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <syscall.h>
#include <sys/ipc.h>
#include <sys/shm.h>

#define CYCLES_PER_US 2396
#define SHM_KEY 401916

#define LDB_MAX_NTHREAD 128

#define DDB_MAX_NLOCK 2048 
#define DDB_MAX_NWAIT 128

#define LDB_MUTEX_EVENT_THRESH_NS 1000

#define barrier() asm volatile("" ::: "memory")
#define CAS(x,y,z) __sync_bool_compare_and_swap(x,y,z)

#ifndef likely
#define likely(x) __builtin_expect(!!(x), 1)
#endif
#ifndef unlikely
#define unlikely(x) __builtin_expect(!!(x), 0)
#endif

extern pthread_key_t ddb_tls_key;

enum ddb_wait_type {
  DDB_WAIT_MUTEX = 1,
  DDB_WAIT_THREAD,
  DDB_WAIT_JOIN, // unused for now
};

typedef struct {
  bool valid;
  uint8_t type;
  uint64_t identifier;  // mutex ptr (uintptr_t) or thread id (pid_t)
}__attribute__((packed, aligned(8))) ddb_wait_entry_t;

typedef struct {
  int64_t n;
  // char pad1[56];
  int64_t max_n;
  // char pad2[56];
  ddb_wait_entry_t *wait_entries;
  ddb_rwlock_t lock;
} ddb_wait_buffer_t;

typedef struct {
  bool valid; // indicates if the thread is still valid
  pid_t id;
  char **fsbase;
  char *stackbase;
  ddb_wait_buffer_t *wbuf;
} ddb_thread_info_t;

typedef struct {
  bool valid;     // if the lock is still valid (valid == false means the lock is released)
  uintptr_t lid; // lock's pointer as its id
  pid_t owner_tid;      // it's owner's tid (can be assigned to other value as long as it can be used to identify the owner)
}__attribute__((packed, aligned(8))) ddb_lowner_entry_t;

typedef struct {
  int64_t n;
  // char pad1[56];
  int64_t max_n;
  // char pad2[56];
  ddb_lowner_entry_t *lowner_entries;
  ddb_rwlock_t lock;
} ddb_lowner_t;

typedef struct {
  ddb_thread_info_t *ddb_thread_infos;
  ddb_lowner_t ddb_lowners;
  int ddb_nthread;
  int ddb_max_idx;
  pthread_spinlock_t ddb_tlock;
} ddb_shmseg;
  

inline __attribute__((always_inline)) void destructor(void* ptr) {
    free(ptr); // Free the memory allocated for the thread-local storage
}

// Helper functions
inline __attribute__((always_inline)) uint64_t rdtsc(void)
{
  uint32_t a, d;
  asm volatile("rdtsc" : "=a" (a), "=d" (d));
  return ((uint64_t)a) | (((uint64_t)d) << 32);
}

inline __attribute__((always_inline)) void cpu_relax(void)
{
  asm volatile("pause");
}

inline __attribute__((always_inline)) void __time_delay_us(uint64_t us)
{
  uint64_t cycles = us * CYCLES_PER_US;
  unsigned long start = rdtsc();

  while (rdtsc() - start < cycles)
    cpu_relax();
}

inline __attribute__((always_inline)) void __time_delay_ns(uint64_t ns)
{
  uint64_t cycles = ns * CYCLES_PER_US / 1000;
  unsigned long start = rdtsc();

  while (rdtsc() - start < cycles)
    cpu_relax();
}

inline __attribute__((always_inline)) char *get_fs_rbp() {
  char *rbp;

  asm volatile ("movq %%fs:-280, %0 \n\t" : "=r"(rbp) :: "memory");

  return rbp;
}

inline __attribute__((always_inline)) char *get_rbp() {
  char *rbp;

  asm volatile ("movq %%rbp, %0 \n\t" : "=r"(rbp) :: "memory");

  return rbp;
}

inline __attribute__((always_inline)) char *rdfsbase() {
  char *fsbase;

  asm volatile ("rdfsbase %0 \n\t" : "=r"(fsbase) :: "memory");

  return fsbase;
}

inline __attribute__((always_inline)) void register_thread_info(pid_t idx) {
  // __asm volatile ("mov %0, %%fs:-208 \n\t" :: "r"(idx): "memory");
  pid_t* tidx = malloc(sizeof(pid_t));
  *tidx = idx;
  pthread_setspecific(ddb_tls_key, tidx);
}

inline __attribute__((always_inline)) pid_t get_thread_info_idx() {
  // __asm volatile ("mov %%fs:-208, %0 \n\t" : "=r"(idx) :: "memory");
  pid_t* value = (pid_t*)pthread_getspecific(ddb_tls_key);
  return *value;
}
