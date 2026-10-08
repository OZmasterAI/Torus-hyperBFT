#!/usr/bin/env bash
# waitfile.sh <file> <max_s>: blocks until file exists or max_s elapses (exit 124).
f=$1 max=${2:-270} t=0
until [ -e "$f" ]; do sleep 10; t=$((t+10)); [ $t -ge $max ] && exit 124; done
cat "$f"
