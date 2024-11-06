#include <pthread.h>
#include <stdio.h>
#include <stdint.h>
#include <unistd.h>

void *thread_a(void *arg) {
    pthread_mutex_t *locks = (pthread_mutex_t *)arg;
    printf("Thread A is running\n");
    pthread_mutex_lock(&locks[0]); // lock Lock1
    printf("Thread A accquired Lock1\n");
    sleep(4);
    pthread_mutex_lock(&locks[1]); // lock Lock2
    printf("Thread A accquired Lock2\n");

    pthread_mutex_unlock(&locks[1]); // unlock Lock2
    pthread_mutex_unlock(&locks[0]); // unlock Lock1
    return NULL;
}

void *thread_b(void *arg) {
    pthread_mutex_t *locks = (pthread_mutex_t *)arg;
    printf("Thread B is running\n");
    sleep(2);
    pthread_mutex_lock(&locks[1]); // lock Lock2
    printf("Thread B accquired Lock2\n");
    sleep(2);
    pthread_mutex_lock(&locks[0]); // lock Lock1
    printf("Thread B accquired Lock1\n");

    pthread_mutex_unlock(&locks[0]); // unlock Lock1
    pthread_mutex_unlock(&locks[1]); // unlock Lock2
    return NULL;
}

int main() {
    pthread_mutex_t lock1 = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_t lock2 = PTHREAD_MUTEX_INITIALIZER;

    pthread_t threada;
    pthread_t threadb;
    pthread_mutex_t locks[2] = {lock1, lock2};
    pthread_create(&threada, NULL, thread_a, locks);
    pthread_create(&threadb, NULL, thread_b, locks);

    pthread_join(threada, NULL);
    pthread_join(threadb, NULL);

    sleep(3);
    // pthread_mutex_lock(&lock);
    // printf("lock_acquired!\n");
    // // Your critical section would go here
    // pthread_mutex_unlock(&lock);
    // printf("Hello, World!\n");
    // sleep(5);
    // shmdt(ddb_shared);
    return 0;
}