#!/usr/bin/env bash
# Tests for tools/dev/torus-target-dir and tools/dev/torus-clean-merged.
# Runs entirely inside a temp HOME with a temp origin + clone + worktrees.
#   bash tools/dev/test_torus_tools.sh
set -uo pipefail

DEV_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TTD="$DEV_DIR/torus-target-dir"
TCM="$DEV_DIR/torus-clean-merged"

PASS=0
FAIL=0
ok()   { PASS=$((PASS + 1)); printf 'ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); printf 'FAIL %s\n' "$1"; [[ -n "${2:-}" ]] && printf '     %s\n' "$2"; }
check_eq() { if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1" "want [$3] got [$2]"; fi; }

TMP="$(mktemp -d)"
TMP="$(realpath "$TMP")"
PIDS=()
finish() {
  local pid
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
  rm -rf -- "${TMP:?}"
}
trap finish EXIT

export HOME="$TMP/home"
mkdir -p "$HOME/projects/wt"
export GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
git config --global init.defaultBranch main
git config --global advice.detachedHead false

# ---------------------------------------------------------------- mapping table
m() { HOME="$HOME" "$TTD" "$1"; }
check_eq "map feat/p00-phase3"        "$(m feat/p00-phase3)"            "$HOME/.cargo-target-p00"
check_eq "map feat/tc-required-a"     "$(m feat/tc-required-phase-a)"   "$HOME/.cargo-target-tc"
check_eq "map fix/tc-foo"             "$(m fix/tc-foo)"                 "$HOME/.cargo-target-tc"
check_eq "map perf/h2h-combined"      "$(m perf/h2h-combined)"          "$HOME/.cargo-target-h2h"
check_eq "map feat/delist-decisions"  "$(m feat/delist-decisions)"      "$HOME/.cargo-target-delist"
check_eq "map fix/delist-x"           "$(m fix/delist-x)"               "$HOME/.cargo-target-delist"
check_eq "map fallback segment"       "$(m perf/mode3-b1/extra)"        "$HOME/.cargo-target-mode3-b1"
check_eq "map fallback sanitised"     "$(m 'feat/Foo Bar$x')"           "$HOME/.cargo-target-foo-bar-x"
check_eq "map no prefix"              "$(m scratch)"                    "$HOME/.cargo-target-scratch"
check_eq "map main -> default"        "$(m main)"                       "$HOME/.cargo-target"
check_eq "map feat/tc-other fallback" "$(m feat/tc-other)"              "$HOME/.cargo-target-tc-other"
check_eq "map refs/heads/ stripped"   "$(m refs/heads/feat/p00-x)"      "$HOME/.cargo-target-p00"

# ---------------------------------------------------------------- repo fixture
ORIGIN="$TMP/origin.git"
REPO="$HOME/projects/Torus-hyperBFT"
WT="$HOME/projects/wt"
git init -q --bare "$ORIGIN"
git clone -q "$ORIGIN" "$REPO" 2>/dev/null
g() { git -C "$REPO" "$@"; }
echo base >"$REPO/f"; g add f; g commit -qm base; g push -q origin main

mkwt() { # name branch
  g worktree add -q -b "$2" "$WT/$1" origin/main 2>/dev/null
}
commit_in() { # dir msg
  echo "$2" >>"$1/f"; git -C "$1" commit -qam "$2"
}
# merged + clean (+ logs dir) -> REMOVE
mkwt merged feat/p00-merged
commit_in "$WT/merged" m1; git -C "$WT/merged" push -q origin feat/p00-merged
g merge -q --no-ff feat/p00-merged -m merge-p00; g push -q origin main
mkdir -p "$WT/merged-logs"; echo log >"$WT/merged-logs/a.log"
# pushed but not merged -> KEEP
mkwt unmerged fix/tc-unmerged
commit_in "$WT/unmerged" u1; git -C "$WT/unmerged" push -q origin fix/tc-unmerged
# merged but dirty -> KEEP
mkwt dirty feat/p00-dirty
echo junk >"$WT/dirty/untracked.txt"
# local commit never pushed -> KEEP
mkwt unpushed feat/p00-unpushed
commit_in "$WT/unpushed" local-only
# merged + clean but a process sits inside -> KEEP
mkwt busy perf/h2h-busy
bash -c "cd '$WT/busy' && exec sleep 300" & PIDS+=($!)
# a worktree outside WT is ignored
g worktree add -q -b feat/outside "$TMP/outside" origin/main 2>/dev/null

# target dirs
mkdir -p "$HOME/.cargo-target-p00/debug" "$HOME/.cargo-target-tc/debug" \
         "$HOME/.cargo-target-h2h/debug" "$HOME/.cargo-target-busyenv/debug" \
         "$HOME/.cargo-target-orphan/debug" "$HOME/.cargo-target/debug" \
         "$HOME/.cargo-target-sccache"
ln -s "$HOME/.cargo-target-orphan" "$HOME/.cargo-target-link"
# p00 has unpushed/dirty branches mapped -> those are unmerged? feat/p00-dirty is at
# origin/main (merged); feat/p00-unpushed is not -> p00 KEEP.
# a branch-free env user: process with CARGO_TARGET_DIR=busyenv
CARGO_TARGET_DIR="$HOME/.cargo-target-busyenv" bash -c 'exec sleep 300' & PIDS+=($!)
sleep 0.3

# ---------------------------------------------------------------- dry run
OUT="$(TORUS_REPO="$REPO" "$TCM" --idle-min 0 2>&1)"
echo "$OUT" | sed 's/^/     | /'
row() { echo "$OUT" | awk -v p="$1" '$1==p'; }
verdict() { row "$1" | awk '{print $3}'; }
check_eq "wt merged clean -> REMOVE"     "$(verdict "$WT/merged")"   REMOVE
check_eq "wt unmerged -> KEEP"           "$(verdict "$WT/unmerged")" KEEP
check_eq "wt dirty -> KEEP"              "$(verdict "$WT/dirty")"    KEEP
check_eq "wt unpushed -> KEEP"           "$(verdict "$WT/unpushed")" KEEP
check_eq "wt busy (cwd) -> KEEP"         "$(verdict "$WT/busy")"     KEEP
row "$WT/unpushed" | grep -q 'unpushed commits' && ok "unpushed reason" || bad "unpushed reason" "$(row "$WT/unpushed")"
row "$WT/dirty" | grep -q 'uncommitted' && ok "dirty reason" || bad "dirty reason"
row "$WT/busy" | grep -q 'process' && ok "busy reason" || bad "busy reason"
check_eq "outside worktree ignored"      "$(row "$TMP/outside")"     ""
check_eq "target p00 (unpushed branch) -> KEEP" "$(verdict "$HOME/.cargo-target-p00")" KEEP
check_eq "target tc (unmerged) -> KEEP"  "$(verdict "$HOME/.cargo-target-tc")"  KEEP
check_eq "target h2h (merged) -> REMOVE" "$(verdict "$HOME/.cargo-target-h2h")" REMOVE
check_eq "target env-used -> KEEP"       "$(verdict "$HOME/.cargo-target-busyenv")" KEEP
row "$HOME/.cargo-target-busyenv" | grep -q CARGO_TARGET_DIR && ok "env reason" || bad "env reason"
check_eq "target orphan -> REMOVE"       "$(verdict "$HOME/.cargo-target-orphan")" REMOVE
check_eq "target symlink -> KEEP"        "$(verdict "$HOME/.cargo-target-link")" KEEP
check_eq "default target not listed"     "$(row "$HOME/.cargo-target")" ""
check_eq "sccache not listed"            "$(row "$HOME/.cargo-target-sccache")" ""
echo "$OUT" | grep -q '^Total reclaimable' && ok "total line" || bad "total line"
[[ -d "$WT/merged" && -d "$HOME/.cargo-target-h2h" ]] && ok "dry run deletes nothing" || bad "dry run deletes nothing"

# idle guard: fresh dirs are KEEP with the default --idle-min
OUT2="$(TORUS_REPO="$REPO" "$TCM" --no-fetch 2>&1)"
echo "$OUT2" | awk -v p="$HOME/.cargo-target-h2h" '$1==p' | grep -q 'KEEP.*written in the last' \
  && ok "idle guard keeps fresh target" || bad "idle guard keeps fresh target"

# idle guard sees writes at any depth (cargo writes .fingerprint/x/y, incremental/...)
mkdir -p "$HOME/.cargo-target-deep/debug/incremental/a/b/c" "$HOME/.cargo-target-stale/debug/deps"
echo x >"$HOME/.cargo-target-stale/debug/deps/old.rlib"
find "$HOME/.cargo-target-deep" "$HOME/.cargo-target-stale" -exec touch -h -d '3 hours ago' {} +
echo fresh >"$HOME/.cargo-target-deep/debug/incremental/a/b/c/fresh.bin"
touch -d '3 hours ago' "$HOME/.cargo-target-deep/debug/incremental/a/b/c" "$HOME/.cargo-target-deep/debug/incremental/a/b"
OUT3="$(TORUS_REPO="$REPO" "$TCM" --no-fetch 2>&1)"
r3() { echo "$OUT3" | awk -v p="$1" '$1==p {print $3}'; }
check_eq "idle guard: deep fresh file -> KEEP" "$(r3 "$HOME/.cargo-target-deep")" KEEP
check_eq "idle guard: all old -> REMOVE"       "$(r3 "$HOME/.cargo-target-stale")" REMOVE

# --cargo: shared dir is locked and invalidated when the owning worktree changes
mkwt pa feat/p00-share-a
mkwt pb feat/p00-share-b
FAKE="$TMP/fake-cargo"
printf '#!/bin/sh\necho "fake-cargo $CARGO_TARGET_DIR $*"\n' >"$FAKE"; chmod +x "$FAKE"
WARN="$(cd "$WT/pa" && "$TTD" 2>&1 >/dev/null)"
[[ "$WARN" == *WARNING*"$WT/pb"* ]] && ok "print mode warns about sharing worktree" || bad "print mode warns" "$WARN"
C1="$(cd "$WT/pa" && TORUS_CARGO="$FAKE" "$TTD" --cargo build -p x 2>&1)"
[[ "$C1" == *touching* && "$C1" == *"fake-cargo $HOME/.cargo-target-p00 build -p x"* ]] \
  && ok "--cargo first run touches + runs cargo in shared dir" || bad "--cargo first run" "$C1"
check_eq "--cargo owner recorded" "$(cat "$HOME/.cargo-target-p00/.torus-owner")" "$WT/pa"
C2="$(cd "$WT/pa" && TORUS_CARGO="$FAKE" "$TTD" --cargo test 2>&1)"
[[ "$C2" != *touching* ]] && ok "--cargo same owner: no touch" || bad "--cargo same owner" "$C2"
touch -d '1 hour ago' "$WT/pb/f"
C3="$(cd "$WT/pb" && TORUS_CARGO="$FAKE" "$TTD" --cargo test 2>&1)"
[[ "$C3" == *touching* && -n "$(find "$WT/pb/f" -mmin -1)" ]] \
  && ok "--cargo owner switch touches sources" || bad "--cargo owner switch" "$C3"
check_eq "--cargo owner switched" "$(cat "$HOME/.cargo-target-p00/.torus-owner")" "$WT/pb"
(cd "$WT/pa" && exec flock "$HOME/.cargo-target-p00/.torus-build.lock" sleep 2) &
LOCKPID=$!
sleep 0.5
C4="$(cd "$WT/pb" && TORUS_CARGO="$FAKE" "$TTD" --cargo test 2>&1)"
[[ "$C4" == *waiting* && "$C4" == *fake-cargo* ]] && ok "--cargo waits on the build lock" || bad "--cargo lock" "$C4"
wait "$LOCKPID"

# ---------------------------------------------------------------- safe_rm guard
mkdir -p "$TMP/elsewhere" "$HOME/projects/wt/a/b"
GUARD="$(HOME="$HOME" bash -c "source '$TCM'; safe_rm '$TMP/elsewhere'; echo rc=\$?" 2>&1)"
[[ -d "$TMP/elsewhere" && "$GUARD" == *REFUSE*rc=1* ]] && ok "safe_rm refuses outside path" || bad "safe_rm refuses outside path" "$GUARD"
GUARD="$(HOME="$HOME" bash -c "source '$TCM'; safe_rm '$HOME/projects/wt/a/b'; echo rc=\$?" 2>&1)"
[[ -d "$HOME/projects/wt/a/b" && "$GUARD" == *REFUSE*rc=1* ]] && ok "safe_rm refuses nested wt path" || bad "safe_rm refuses nested wt path" "$GUARD"
GUARD="$(HOME="$HOME" bash -c "source '$TCM'; safe_rm '$HOME/.cargo-target'; echo rc=\$?" 2>&1)"
[[ -d "$HOME/.cargo-target" && "$GUARD" == *REFUSE*rc=1* ]] && ok "safe_rm refuses default target" || bad "safe_rm refuses default target" "$GUARD"
GUARD="$(HOME="$HOME" bash -c "source '$TCM'; safe_rm '$HOME/.cargo-target-link'; echo rc=\$?" 2>&1)"
[[ -d "$HOME/.cargo-target-orphan" && "$GUARD" == *REFUSE*rc=1* ]] && ok "safe_rm refuses symlink" || bad "safe_rm refuses symlink" "$GUARD"
GUARD="$(HOME="$HOME" bash -c "source '$TCM'; safe_rm ''; echo rc=\$?" 2>&1)"
[[ "$GUARD" == *"empty path"* ]] && ok "safe_rm refuses empty" || bad "safe_rm refuses empty" "$GUARD"

# ---------------------------------------------------------------- apply
OUT="$(TORUS_REPO="$REPO" "$TCM" --no-fetch --idle-min 0 --apply 2>&1)"
[[ ! -e "$WT/merged" ]] && ok "apply removed merged worktree" || bad "apply removed merged worktree" "$OUT"
[[ ! -e "$WT/merged-logs" ]] && ok "apply removed merged logs dir" || bad "apply removed merged logs dir"
[[ ! -e "$HOME/.cargo-target-h2h" ]] && ok "apply removed merged target" || bad "apply removed merged target"
[[ ! -e "$HOME/.cargo-target-orphan" ]] && ok "apply removed orphan target" || bad "apply removed orphan target"
for d in "$WT/unmerged" "$WT/dirty" "$WT/unpushed" "$WT/busy" "$HOME/.cargo-target-p00" \
         "$HOME/.cargo-target-tc" "$HOME/.cargo-target-busyenv" "$HOME/.cargo-target" \
         "$HOME/.cargo-target-sccache" "$TMP/outside"; do
  [[ -e "$d" ]] && ok "apply kept ${d#"$TMP"/}" || bad "apply kept ${d#"$TMP"/}"
done
if g worktree list --porcelain | grep -q "$WT/merged\$"; then bad "worktree unregistered"; else ok "worktree unregistered"; fi
[[ -f "$WT/dirty/untracked.txt" ]] && ok "dirty file untouched" || bad "dirty file untouched"

echo
echo "passed: $PASS  failed: $FAIL"
(( FAIL == 0 ))
