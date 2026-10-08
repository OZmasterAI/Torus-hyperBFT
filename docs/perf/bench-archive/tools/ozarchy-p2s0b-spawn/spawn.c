// spawn+join cost of a scope of K threads (2 MiB stacks like Rust std), trivial body; ns per thread and per scope.
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
static void *body(void *a) { volatile long x = (long)a; x++; return NULL; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e9 + t.tv_nsec; }
int main(int argc, char **argv) {
    int K = atoi(argv[1]), R = atoi(argv[2]);
    pthread_attr_t at; pthread_attr_init(&at); pthread_attr_setstacksize(&at, 2 << 20);
    pthread_t th[256]; double best = 1e18, sum = 0;
    for (int r = 0; r < R; r++) {
        double t0 = now();
        for (int i = 0; i < K; i++) pthread_create(&th[i], &at, body, (void *)(long)i);
        for (int i = 0; i < K; i++) pthread_join(th[i], NULL);
        double d = now() - t0; sum += d; if (d < best) best = d;
    }
    printf("K=%d scopes=%d mean_us_per_scope=%.1f best_us_per_scope=%.1f mean_us_per_thread=%.2f\n", K, R, sum / R / 1e3, best / 1e3, sum / R / K / 1e3);
    return 0;
}
