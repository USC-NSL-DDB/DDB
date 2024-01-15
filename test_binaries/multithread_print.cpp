#include <iostream>
#include <cstdlib>
#include <pthread.h>
#include <unistd.h>

#define NUM_THREADS 5
#define MAX_LOOP 15

void *say_hi(void *thread_id) {
    long tid = (long) thread_id;
    for (size_t i = 0; i < MAX_LOOP; i++) {
	    size_t sleep_duration = rand() % 5;
        printf("Hello World - thread %lu, loop %lu. Will sleep %lu seconds.\n", tid, i, sleep_duration);
        sleep(sleep_duration);
    }
    printf("Thread %lu exits.\n", tid);
    pthread_exit(NULL);
}

int main() {
	srand((unsigned) time(NULL));

    std::cout << "Hello, World! from start!" << std::endl;

    pthread_t threads[NUM_THREADS];

    for (int i = 0; i < NUM_THREADS; i++) {
        printf("main(): creating thread - %d\n", i);

        int r = pthread_create(&threads[i], NULL, say_hi, (void*)i);
        if (r) {
            printf("Failed to create a thread, errno: %d\n", r);
            exit(-1);
        }
    }

    for (const auto tid: threads) {
        pthread_join(tid, NULL);
    }

    pthread_exit(NULL);
    return 0;
}
