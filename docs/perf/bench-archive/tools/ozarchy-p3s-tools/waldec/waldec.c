// RocksDB WAL decoder (legacy, non-recyclable record format; recycle_log_file_num=0, no WAL compression).
// Usage: waldec OUTDIR file1.log file2.log ...   (files in log-number order)
// Writes OUTDIR/batches.bin, OUTDIR/entries.bin and prints validation stats to stdout.
// Physical record: crc32c(masked, over type+payload) u32 | len u16 | type u8 | payload. 32 KiB blocks.
// Logical record = WriteBatch: seq u64 | count u32 | entries (tag, [cf varint], key slice, [value slice]).
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>

static uint32_t crctab[256];
static void crcinit(void){ for(uint32_t i=0;i<256;i++){uint32_t c=i; for(int k=0;k<8;k++) c = (c&1)? (c>>1)^0x82F63B78u : c>>1; crctab[i]=c;} }
static uint32_t crc32c(uint32_t crc,const uint8_t*p,size_t n){ crc=~crc; while(n--) crc=crctab[(crc^*p++)&0xff]^(crc>>8); return ~crc; }
static uint32_t unmask(uint32_t m){ uint32_t r=m-0xa282ead8u; return (r>>17)|(r<<15); }

#pragma pack(push,1)
typedef struct { uint64_t seq; uint32_t count; uint32_t file_no; uint64_t applied_h; uint64_t payload_bytes; uint32_t n_entries; uint32_t flags; } Batch; // 40 B
typedef struct { uint32_t batch; uint8_t cf; uint8_t op; uint16_t pad; uint32_t klen; uint32_t vlen; uint64_t khash; } Entry; // 24 B
#pragma pack(pop)

static FILE *fb,*fe;
static uint64_t nbatches=0, nentries=0, bad_crc=0, bad_count=0, seq_gaps=0, parse_err=0, frag_err=0, trunc_tail=0, unknown_tag=0, logdata=0, nonzero_pad=0;
static uint64_t last_seq_end=0; static int have_seq=0;
static uint64_t ops[32];

static int varint32(const uint8_t**p,const uint8_t*e,uint32_t*out){ uint32_t r=0; for(int s=0;s<=28 && *p<e;s+=7){ uint8_t b=*(*p)++; r|=(uint32_t)(b&0x7f)<<s; if(!(b&0x80)){*out=r;return 1;} } return 0; }
static int slice(const uint8_t**p,const uint8_t*e,const uint8_t**s,uint32_t*n){ if(!varint32(p,e,n)) return 0; if((size_t)(e-*p)<*n) return 0; *s=*p; *p+=*n; return 1; }
static uint64_t hkey(uint8_t cf,const uint8_t*k,uint32_t n){ uint64_t h=1469598103934665603ULL ^ (cf*0x9E3779B97F4A7C15ULL); for(uint32_t i=0;i<n;i++){ h^=k[i]; h*=1099511628211ULL; } h^=h>>33; h*=0xff51afd7ed558ccdULL; h^=h>>33; h*=0xc4ceb9fe1a85ec53ULL; h^=h>>33; return h; }

static const char META_AH[]="native_applied_height";

static void batch_done(const uint8_t*r,size_t n,uint32_t file_no){
    if(n<12){ parse_err++; return; }
    Batch b; memset(&b,0,sizeof b);
    memcpy(&b.seq,r,8); memcpy(&b.count,r+8,4); b.file_no=file_no; b.applied_h=UINT64_MAX; b.payload_bytes=n;
    if(have_seq && b.seq!=last_seq_end){ seq_gaps++; if(seq_gaps<20) fprintf(stderr,"seq gap file %u: expected %lu got %lu\n",file_no,(unsigned long)last_seq_end,(unsigned long)b.seq); }
    const uint8_t*p=r+12,*e=r+n; uint32_t counted=0;
    while(p<e){
        uint8_t tag=*p++; uint32_t cf=0; const uint8_t*k=0,*v=0; uint32_t kl=0,vl=0; int counts=1, haskey=1, hasval=0;
        switch(tag){
        case 0x1: hasval=1; break;              // Value (default cf)
        case 0x0: case 0x7: break;              // Deletion, SingleDeletion
        case 0x2: hasval=1; break;              // Merge
        case 0x5: case 0x6: case 0x19: case 0x10: case 0x17: if(!varint32(&p,e,&cf)){parse_err++;goto out;} hasval=1; break; // CF value/merge/...
        case 0x4: case 0x8: if(!varint32(&p,e,&cf)){parse_err++;goto out;} break; // CF deletion / single deletion
        case 0xE: if(!varint32(&p,e,&cf)){parse_err++;goto out;} hasval=1; break; // CF range deletion (begin,end)
        case 0xF: hasval=1; break;
        case 0x18: case 0x11: case 0x16: hasval=1; break;
        case 0x3: { const uint8_t*s; uint32_t sl; if(!slice(&p,e,&s,&sl)){parse_err++;goto out;} logdata++; counts=0; haskey=0; continue; }
        case 0xD: case 0x9: case 0x12: case 0x13: counts=0; haskey=0; continue;
        case 0xA: case 0xB: case 0xC: { const uint8_t*s; uint32_t sl; if(!slice(&p,e,&s,&sl)){parse_err++;goto out;} continue; }
        default: unknown_tag++; parse_err++; goto out;
        }
        if(haskey && !slice(&p,e,&k,&kl)){ parse_err++; goto out; }
        if(hasval && !slice(&p,e,&v,&vl)){ parse_err++; goto out; }
        if(counts) counted++;
        ops[tag&31]++;
        Entry en; en.batch=(uint32_t)nbatches; en.cf=(uint8_t)cf; en.op=tag; en.pad=0; en.klen=kl; en.vlen=vl; en.khash=hkey((uint8_t)cf,k,kl);
        if(cf==36 && kl==sizeof(META_AH)-1 && memcmp(k,META_AH,kl)==0 && vl==8 && (tag==0x5)){ uint64_t h=0; for(int i=0;i<8;i++) h=(h<<8)|v[i]; b.applied_h=h; }
        fwrite(&en,sizeof en,1,fe); nentries++; b.n_entries++;
    }
out:
    if(counted!=b.count){ bad_count++; if(bad_count<20) fprintf(stderr,"count mismatch batch %lu: header %u parsed %u\n",(unsigned long)nbatches,b.count,counted); }
    last_seq_end=b.seq+b.count; have_seq=1;
    fwrite(&b,sizeof b,1,fb); nbatches++;
}

int main(int argc,char**argv){
    if(argc<3){ fprintf(stderr,"usage\n"); return 2; }
    crcinit();
    char path[4096];
    snprintf(path,sizeof path,"%s/batches.bin",argv[1]); fb=fopen(path,"wb");
    snprintf(path,sizeof path,"%s/entries.bin",argv[1]); fe=fopen(path,"wb");
    if(!fb||!fe){ perror("open out"); return 2; }
    static uint8_t *buf=0; size_t cap=0, blen=0; int infrag=0;
    for(int a=2;a<argc;a++){
        const char*fn=argv[a]; const char*base=strrchr(fn,'/'); base=base?base+1:fn; uint32_t file_no=(uint32_t)strtoul(base,0,10);
        int fd=open(fn,O_RDONLY); struct stat st; fstat(fd,&st); size_t sz=st.st_size;
        if(sz==0){ close(fd); printf("file %s empty\n",base); continue; }
        uint8_t*m=mmap(0,sz,PROT_READ,MAP_PRIVATE,fd,0); madvise(m,sz,MADV_SEQUENTIAL);
        size_t off=0; uint64_t fb0=nbatches; int stop=0;
        if(infrag){ frag_err++; infrag=0; blen=0; } // fragments never span files
        while(off<sz && !stop){
            size_t left=32768-(off%32768);
            if(left<7){ for(size_t i=0;i<left && off+i<sz;i++) if(m[off+i]) nonzero_pad++; off+=left; continue; }
            if(off+7>sz){ trunc_tail++; break; }
            uint32_t mc; uint16_t ln; uint8_t t; memcpy(&mc,m+off,4); memcpy(&ln,m+off+4,2); t=m[off+6];
            if(t==0 && ln==0 && mc==0){ // zero padding / preallocated tail
                // skip rest of block; if rest of file is zero, stop
                off+=left; continue; }
            if(off+7+ln>sz || 7+(size_t)ln>left){ trunc_tail++; break; }
            const uint8_t*d=m+off+7;
            uint32_t c=crc32c(0,&t,1); c=crc32c(c,d,ln);
            if(c!=unmask(mc)){ bad_crc++; fprintf(stderr,"bad crc file %u off %zu type %u len %u\n",file_no,off,t,ln); off+=left; infrag=0; blen=0; continue; }
            if(blen+ln>cap){ cap=(blen+ln)*2+65536; buf=realloc(buf,cap); }
            switch(t){
            case 1: if(infrag){frag_err++;} infrag=0; blen=0; batch_done(d,ln,file_no); break;
            case 2: if(infrag){frag_err++;} infrag=1; blen=0; memcpy(buf,d,ln); blen=ln; break;
            case 3: if(!infrag){frag_err++; break;} memcpy(buf+blen,d,ln); blen+=ln; break;
            case 4: if(!infrag){frag_err++; break;} memcpy(buf+blen,d,ln); blen+=ln; batch_done(buf,blen,file_no); infrag=0; blen=0; break;
            default: fprintf(stderr,"record type %u at file %u off %zu\n",t,file_no,off); break;
            }
            off+=7+ln;
        }
        printf("file %06u bytes %zu batches %lu\n",file_no,sz,(unsigned long)(nbatches-fb0));
        munmap(m,sz); close(fd);
    }
    fclose(fb); fclose(fe);
    printf("TOTAL batches %lu entries %lu bad_crc %lu bad_count %lu seq_gaps %lu parse_err %lu frag_err %lu trunc_tail %lu unknown_tag %lu logdata %lu nonzero_pad %lu last_seq_end %lu\n",
      (unsigned long)nbatches,(unsigned long)nentries,(unsigned long)bad_crc,(unsigned long)bad_count,(unsigned long)seq_gaps,(unsigned long)parse_err,(unsigned long)frag_err,(unsigned long)trunc_tail,(unsigned long)unknown_tag,(unsigned long)logdata,(unsigned long)nonzero_pad,(unsigned long)last_seq_end);
    printf("ops:"); for(int i=0;i<32;i++) if(ops[i]) printf(" 0x%x=%lu",i,(unsigned long)ops[i]); printf("\n");
    return 0;
}
