// MWC1616 (V8 4.5 / Node 4.x Math.random) recovery + linking for stock Nyuu Message-IDs.
// Input (stdin): one line per ID: "<24 letters> <timestamp>"  (any order). Output: per-ID stream offsets, stats.
// Letter = floor(x*26), x = ((r0<<16) + (r1&0xFFFF)) / 2^32 ; top 16 bits of x == low16(r0) exactly (no carry),
// so letters depend on r0 only, except when a letter boundary falls inside the 2^16 window set by r1 (~0.04%/draw):
// we accept any letter in [lo,hi] for that window (tolerant), and use the "nominal" letter for hashing.
// Build: gcc -O3 -pthread -o mwc_link mwc_link.c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <pthread.h>
#include <time.h>
#define NL 24
static inline uint32_t step(uint32_t r){ return 18030u*(r&0xFFFF) + (r>>16); }
static inline int lo_l(uint32_t r){ return (int)(((uint64_t)((r&0xFFFF)<<16)*26)>>32); }
static inline int hi_l(uint32_t r){ return (int)(((uint64_t)(((r&0xFFFF)<<16)|0xFFFF)*26)>>32); }
static inline int nom_l(uint32_t r){ return (int)(((uint64_t)(((r&0xFFFF)<<16)|0x8000)*26)>>32); }
typedef struct { int v[NL]; long long ts; int pos; int grp; uint32_t r0; } ID;
static ID *ids; static int n;
// brute force: r0 = state after the ID's first draw; r0 < 18030*65536 after any step.
typedef struct { int *v; uint32_t H0,H1; uint32_t found[8]; int nf; } Job;
static void *bf(void *a){ Job *j=a; int *v=j->v;
  for(uint32_t L=0; L<65536; L++){ uint32_t t=L<<16; int l=(int)(((uint64_t)t*26)>>32), h=(int)(((uint64_t)(t|0xFFFF)*26)>>32);
    if(v[0]<l||v[0]>h) continue;
    for(uint32_t H=j->H0; H<j->H1; H++){ uint32_t r=(H<<16)|L, s=r; int k;
      for(k=1;k<NL;k++){ s=step(s); if(v[k]<lo_l(s)||v[k]>hi_l(s)) break; }
      if(k==NL && j->nf<8) j->found[j->nf++]=r; } }
  return 0; }
static int solve(int *v, uint32_t *out, int T){ pthread_t th[64]; Job J[64]; uint32_t HM=18030; int nf=0;
  for(int t=0;t<T;t++){ J[t].v=v; J[t].H0=HM*t/T; J[t].H1=HM*(t+1)/T; J[t].nf=0; pthread_create(&th[t],0,bf,&J[t]); }
  for(int t=0;t<T;t++){ pthread_join(th[t],0); for(int i=0;i<J[t].nf&&nf<8;i++) out[nf++]=J[t].found[i]; }
  return nf; }
static int cmpts(const void*a,const void*b){ const ID*x=a,*y=b; return x->ts<y->ts?-1:x->ts>y->ts; }
static int match_at(uint32_t r, int *v){ for(int k=0;k<NL;k++){ if(k) r=step(r); if(v[k]<lo_l(r)||v[k]>hi_l(r)) return 0; } return 1; }
int main(int argc,char**argv){ int T=argc>1?atoi(argv[1]):8; char s[256]; long long ts; int cap=1024; ids=malloc(cap*sizeof(ID));
  while(scanf("%255s %lld",s,&ts)==2){ if(strlen(s)!=NL) continue; if(n==cap){cap*=2; ids=realloc(ids,cap*sizeof(ID));}
    for(int k=0;k<NL;k++) ids[n].v[k]=s[k]-((k&1)?'a':'A'); ids[n].ts=ts; ids[n].pos=-1; ids[n].grp=-1; n++; }
  qsort(ids,n,sizeof(ID),cmpts);
  long W = 200L*n + 20000; // forward window in draws
  uint32_t *st=malloc((W+NL)*sizeof(uint32_t));
  int grp=0, linked=0, fails=0, tried=0; double tbf=0; clock_t c0=clock(); struct timespec a0,a1; clock_gettime(CLOCK_MONOTONIC,&a0);
  // hash of nominal 6-letter prefix -> list of stream positions (rebuilt per group)
  int HS=1<<22; int *head=malloc(HS*sizeof(int)); int *nxt=malloc((W+1)*sizeof(int));
  for(int i=0;i<n;i++){ if(ids[i].pos>=0) continue;
    uint32_t c[8]; struct timespec b0,b1; clock_gettime(CLOCK_MONOTONIC,&b0); int nf=solve(ids[i].v,c,T); clock_gettime(CLOCK_MONOTONIC,&b1);
    tbf+=(b1.tv_sec-b0.tv_sec)+(b1.tv_nsec-b0.tv_nsec)/1e9;
    tried++; if(nf!=1){ fprintf(stderr,"ID %d: %d candidates\n",i,nf); if(nf==0){ if(++fails>=8) break; continue; } }
    ids[i].r0=c[0]; ids[i].pos=0; ids[i].grp=grp; int gl=1;
    st[0]=c[0]; for(long p=1;p<W+NL;p++) st[p]=step(st[p-1]);
    memset(head,-1,HS*sizeof(int));
    for(long p=0;p<W;p++){ uint32_t h=0,r; for(int k=0;k<6;k++){ r=st[p+k]; h=h*26+nom_l(r);} h&=HS-1; nxt[p]=head[h]; head[h]=p; }
    for(int j=i+1;j<n;j++){ if(ids[j].pos>=0) continue; uint32_t h=0; for(int k=0;k<6;k++) h=h*26+ids[j].v[k]; h&=HS-1;
      for(int p=head[h]; p>=0; p=nxt[p]) if(match_at(st[p],ids[j].v)){ ids[j].pos=p; ids[j].grp=grp; ids[j].r0=st[p]; gl++; break; } }
    printf("group %d: anchor ts %lld r0=%08x linked %d IDs\n",grp,ids[i].ts,c[0],gl); linked+=gl; grp++;
    if(grp>=200) break; }
  clock_gettime(CLOCK_MONOTONIC,&a1);
  // gap histogram within group 0..: sort positions
  int hist[8]={0}; long other=0; int *ps=malloc(n*sizeof(int));
  for(int g=0;g<grp;g++){ int m=0; for(int i=0;i<n;i++) if(ids[i].grp==g) ps[m++]=ids[i].pos;
    for(int a=1;a<m;a++){ int x=ps[a],k=a; while(k>0&&ps[k-1]>x){ps[k]=ps[k-1];k--;} ps[k]=x; }
    for(int a=1;a<m;a++){ int d=ps[a]-ps[a-1]; if(d%24==0 && d/24<8) hist[d/24]++; else other++; } }
  printf("SUMMARY bf_tried %d zero_cand %d IDs %d linked %d groups %d (singletons counted) bf_secs %.2f total_secs %.2f\n",tried,fails,n,linked,grp,tbf,(a1.tv_sec-a0.tv_sec)+(a1.tv_nsec-a0.tv_nsec)/1e9);
  printf("gap hist (draws): 24:%d 48:%d 72:%d 96:%d 120:%d 144:%d 168:%d other:%ld\n",hist[1],hist[2],hist[3],hist[4],hist[5],hist[6],hist[7],other);
  return 0; }
