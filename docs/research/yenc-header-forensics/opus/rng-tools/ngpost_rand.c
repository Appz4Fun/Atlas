// glibc random() TYPE_3 reimplementation + ngPost From brute force benchmark.
// build: gcc -O3 -march=native -fopenmp ngpost_rand.c -o ngpost_rand
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <omp.h>

static const char AL[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

typedef struct { int32_t r[34]; int i; } g3;   // ring of last 34 values
static inline void g3_seed(g3 *g, uint32_t seed) {
    int32_t *r = g->r;
    if (seed == 0) seed = 1;                 // glibc: seed 0 -> 1
    r[0] = (int32_t)seed;
    for (int i = 1; i < 31; i++) {           // Schrage: 16807*r % 2147483647
        int32_t hi = r[i-1] / 127773, lo = r[i-1] % 127773;
        int32_t w = 16807 * lo - 2836 * hi;
        if (w < 0) w += 2147483647;
        r[i] = w;
    }
    for (int i = 31; i < 34; i++) r[i] = r[i-31];
    g->i = 34;
    // discard 310: compute in ring of size 34 indices mod 34
    for (int k = 0; k < 310; k++) {
        int n = g->i;
        r[n % 34] = (int32_t)((uint32_t)r[(n-31) % 34] + (uint32_t)r[(n-3) % 34]);
        g->i++;
    }
}
static inline int32_t g3_next(g3 *g) {
    int n = g->i;
    uint32_t v = (uint32_t)g->r[(n-31) % 34] + (uint32_t)g->r[(n-3) % 34];
    g->r[n % 34] = (int32_t)v;
    g->i++;
    return (int32_t)(v >> 1);
}

static void from_of(g3 *g, char *out) { for (int i = 0; i < 13; i++) out[i] = AL[g3_next(g) % 62]; out[13] = 0; }

int main(int argc, char **argv) {
    // 1) verify vs system srand/rand
    uint32_t seeds[] = {1, 2, 42, 12345, 0xDEADBEEF, 0, 4294967295u};
    int bad = 0;
    for (unsigned s = 0; s < sizeof seeds / sizeof *seeds; s++) {
        g3 g; g3_seed(&g, seeds[s]); srand(seeds[s]);
        for (int k = 0; k < 100000; k++) if (g3_next(&g) != rand()) { bad++; break; }
    }
    printf("verify vs glibc rand(): %s\n", bad ? "MISMATCH" : "OK (7 seeds x 100000 outputs)");
    { g3 g; g3_seed(&g, 1); char f[14]; printf("glibc seed1 Froms:"); for (int i = 0; i < 5; i++) { from_of(&g, f); printf(" %s", f);} printf("\n"); }

    // MSVC seed-1 LCG
    { uint32_t x = 1; printf("MSVC seed1 Froms:"); for (int i = 0; i < 5; i++) { char f[14];
        for (int j = 0; j < 13; j++) { x = x * 214013u + 2531011u; f[j] = AL[((x >> 16) & 0x7fff) % 62]; } f[13] = 0; printf(" %s", f);} printf("\n"); }

    // 2) benchmark: target From from a secret seed
    uint32_t secret = 0x00071234u; // inside 2^20 range
    char tgt0[14], tgtK[14];
    { g3 g; g3_seed(&g, secret); from_of(&g, tgt0); for (int i = 1; i < 700; i++) from_of(&g, tgtK); }
    uint32_t NS = 1u << 20;
    int thr = omp_get_max_threads();
    // (a) index 0, single thread
    double t = omp_get_wtime(); uint32_t found = 0;
    for (uint32_t s = 0; s < NS; s++) { g3 g; g3_seed(&g, s); char f[14]; from_of(&g, f); if (!memcmp(f, tgt0, 13)) found = s; }
    double ta = omp_get_wtime() - t;
    printf("[idx0, 1 core] 2^20 seeds in %.3fs, found=%#x -> 2^32 est %.1f s (1 core), %.1f s (32 cores)\n", ta, found, ta * 4096, ta * 4096 / 32);
    // (b) within first 1000 Froms (13000 outputs/seed), first-char prefilter
    t = omp_get_wtime(); found = 0; int fidx = -1;
    #pragma omp parallel for schedule(dynamic, 4096)
    for (uint32_t s = 0; s < NS; s++) {
        g3 g; g3_seed(&g, s);
        for (int i = 0; i < 1000; i++) {
            char f[14]; from_of(&g, f);
            if (!memcmp(f, tgtK, 13)) {
                #pragma omp critical
                { found = s; fidx = i; }
            }
        }
    }
    double tb = omp_get_wtime() - t;
    printf("[1000 Froms, %d threads] 2^20 seeds in %.2fs, found=%#x at From #%d -> per-core-seconds %.1f; 2^32 est %.0f s (1 core) %.0f s (32 cores)\n",
           thr, tb, found, fidx, tb * thr, tb * thr * 4096, tb * thr * 4096 / 32);
    return 0;
}
