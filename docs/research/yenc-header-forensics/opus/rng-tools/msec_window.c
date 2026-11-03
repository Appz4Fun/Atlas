// Pre-v4.15 ngPost: srand((uint)currentMSecsSinceEpoch). Given article Date (unix s), search [Date-86400s, Date+86400s] ms seeds.
// build: gcc -O3 -march=native -fopenmp msec_window.c -o msec_window ; usage: ./msec_window <date_unix> <true_ms_offset_for_demo>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <omp.h>
static const char AL[]="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
typedef struct{int32_t r[34];int i;}g3;
static inline void g3_seed(g3*g,uint32_t s){int32_t*r=g->r;if(!s)s=1;r[0]=s;for(int i=1;i<31;i++){int32_t hi=r[i-1]/127773,lo=r[i-1]%127773,w=16807*lo-2836*hi;if(w<0)w+=2147483647;r[i]=w;}for(int i=31;i<34;i++)r[i]=r[i-31];g->i=34;for(int k=0;k<310;k++){int n=g->i;r[n%34]=(int32_t)((uint32_t)r[(n-31)%34]+(uint32_t)r[(n-3)%34]);g->i++;}}
static inline int32_t g3_next(g3*g){int n=g->i;uint32_t v=(uint32_t)g->r[(n-31)%34]+(uint32_t)g->r[(n-3)%34];g->r[n%34]=v;g->i++;return v>>1;}
int main(int c,char**v){
  int64_t date=atoll(v[1]); int64_t off=atoll(v[2]);
  int64_t lo=(date-86400)*1000, hi=(date+86400)*1000;
  printf("Date=%lld ms-range [%lld,%lld] = %lld candidates; as uint seeds [%u .. %u]%s\n",(long long)date,(long long)lo,(long long)hi,(long long)(hi-lo),
     (uint32_t)lo,(uint32_t)hi, ((uint32_t)lo>(uint32_t)hi)?" (wraps)":"");
  int64_t truems=date*1000+off; char tgt[14]; {g3 g;g3_seed(&g,(uint32_t)truems);for(int i=0;i<13;i++)tgt[i]=AL[g3_next(&g)%62];tgt[13]=0;}
  printf("demo target From %s@ngPost.com (seed ms %lld)\n",tgt,(long long)truems);
  double t=omp_get_wtime(); int64_t hit=-1; long nh=0;
  #pragma omp parallel for schedule(dynamic,65536) reduction(+:nh)
  for(int64_t ms=lo;ms<hi;ms++){g3 g;g3_seed(&g,(uint32_t)ms);char f[13];for(int i=0;i<13;i++)f[i]=AL[g3_next(&g)%62];if(!memcmp(f,tgt,13)){nh++;hit=ms;}}
  printf("searched in %.1fs on %d threads: hits=%ld last=%lld (correct=%d)\n",omp_get_wtime()-t,omp_get_max_threads(),nh,(long long)hit,hit==truems);
}
