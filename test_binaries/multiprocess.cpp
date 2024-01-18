#include <iostream>
#include <unistd.h>
#include <sys/wait.h>
#include <pthread.h>

#define NUM_PROC 3
#define NUM_THREADS 5
#define LOOP_COUNT 100
#define MULTITHREAD true

struct PrintArgs {
    pid_t pid;
    pthread_t tid;
};

void *let_print(void* args) {
    PrintArgs* casted_args = static_cast<PrintArgs*>(args);

    pid_t pid = casted_args->pid;
    pthread_t tid = casted_args->tid;

    for (int i = 0; i < LOOP_COUNT; i++) {
	    size_t sleep_duration = rand() % 5;
        printf("[CHILD] pid: %d, tid: %lu, loop %d. Will sleep %lu seconds.\n", pid, tid, i, sleep_duration);
        sleep(sleep_duration);
    }
    printf("Thread %lu exits.\n", tid);
    pthread_exit(NULL);
}

void simple_print(int pid) {
    for (int j = 0; j < LOOP_COUNT; j++) {
	    size_t sleep_duration = rand() % 5;
        printf("[CHILD] pid: %d, loop %d. Will sleep %lu seconds.\n", pid, j, sleep_duration);
        sleep(sleep_duration);
    }
}

void run_in_multithread(int pid) {
    pthread_t threads[NUM_THREADS];

    for (int i = 0; i < NUM_THREADS; i++) {
        PrintArgs args;
        args.pid = pid;
        args.tid = i;

        int r = pthread_create(&threads[i], NULL, let_print, &args);
        if (r) {
            printf("Failed to create a thread, errno: %d\n", r);
            exit(-1);
        }
    }

    for (const auto tid: threads) {
        pthread_join(tid, NULL);
    }

    pthread_exit(NULL);
}

int main() {
    // not gonna use the real pid and tid

    for (int i = 0; i < NUM_PROC; ++i) {
        pid_t pid = fork();

        if (pid == -1) {
            // If fork() returns -1, an error occurred
            std::cerr << "Error creating process" << std::endl;
            return 1;
        } else if (pid == 0) {
            // Child
            if (MULTITHREAD) {
                run_in_multithread(i + 1);
            } else {
                simple_print(i + 1);
            }
            return 0;
        }
    }

    // Parent
    pid_t pid = 0;
    for (int j = 0; j < LOOP_COUNT; j++) {
	    size_t sleep_duration = rand() % 5;
        printf("[PARENT] pid: %d, loop %d. Will sleep %lu seconds.\n", pid, j, sleep_duration);
        sleep(sleep_duration);
    }

    // Wait 
    for (int i = 0; i < NUM_PROC; ++i) {
        wait(NULL);
    }

    std::cout << "Parent process: All child processes have finished." << std::endl;

    return 0;
}
