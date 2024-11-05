#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <unistd.h>

    void *thread_function(void *arg) {
        printf("Thread is running\n");
        sleep(10);
        return NULL;
    }

int main() {
    // pid_t pid = getpid();
    // pthread_t tid = pthread_self();
    // printf("Process ID: %d, Thread ID: %lu\n", pid, (unsigned long)tid);
    // int shmid = shmget(SHM_KEY, sizeof(ddb_shmseg), 0666 | IPC_CREAT);
    // ddb_shared = shmat(shmid, NULL, 0);
    // ddb_shared->ddb_thread_infos = (ddb_thread_info_t *)malloc(sizeof(ddb_thread_info_t) * 20);
    pthread_t thread;
    pthread_create(&thread, NULL, thread_function, NULL);
    sleep(3);

    pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_lock(&lock);
    // Your critical section would go here
    pthread_mutex_unlock(&lock);
    printf("Hello, World!\n");
    // shmdt(ddb_shared);
    return 0;
}