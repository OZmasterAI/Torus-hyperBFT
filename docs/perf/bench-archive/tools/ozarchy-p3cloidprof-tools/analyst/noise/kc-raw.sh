#!/usr/bin/env bash
# kc-raw.sh: Keccak-leaf samples that still carry a user caller chain (~2% of them), per cell, by thread | caller |
# caller's caller, summed sample period. Writes keccak-intact-chain-callers.raw (input of a-kc.py) to stdout.
# The awk is the analyst's inline command from the 2026-10-13 noise re-analysis (section 60.6), saved as a script for
# the archive; only the cell loop and paths are parameterised. Read-only on <cell>/prof.script.gz and <cell>/exec-tid.txt.
# awk's `for (k in c)` order is unspecified: compare outputs with `sort`.
# Usage: kc-raw.sh <bench-results-matched dir> > keccak-intact-chain-callers.raw
set -euo pipefail
B=${1:?usage: kc-raw.sh <bench-results-matched dir>}
for t in r1-c0 r2-c0 r3-c0 r1-c100 r2-c100 r3-c100; do
  c="$B/ozarchy-p3cloidprof-300m-$t"
  zcat "$c/prof.script.gz" | nice -n 19 awk -v T="$t" -v ET="$(cat "$c/exec-tid.txt")" 'BEGIN{RS=""} { n=split($0,L,"\n"); split(L[1],h," "); u=0; for(i=2;i<=n;i++){ if (L[i] !~ /^[ \t]+ffffffff/) {u=i;break} } if(u && L[u] ~ /Keccak|KECCAK|keccak256/ && n-u>=1){ th=(h[2]==ET)?"EXEC":h[1]; c1="";c2=""; for(i=u+1;i<=n;i++){x=L[i]; sub(/^[ \t]+[0-9a-f]+ /,"",x); sub(/\+0x[0-9a-f]+$/,"",x); if (x ~ /Keccak|KECCAK|keccak256|sha3|Sha3|Digest/) continue; if(c1==""){c1=x; continue} c2=x; break} c[th " | " substr(c1,1,60) " | " substr(c2,1,70)]+=h[3] } } END{for(k in c) print T, c[k], k}'
done
