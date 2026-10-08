#!/usr/bin/env bash
# Gate 2 B-blind prep (ozarchy): candidate worktree + own cargo target dir, build, stage main. Starts NO node.
#   ozarchy-bblind-gate2-prep.sh <CANDIDATE_SHA>
# Candidate = the B-blind commit on origin/perf/item6-phase1 (must contain ab12c75). Main = 92a02ed, binary pinned by md5.
# Flags = main 31a95c65's (as ozarchy-4acdc59-build/build.sh): mold, frame pointers, line-tables-only.
# Writes $R/ozarchy-bblind-<s7>-build/{build.log,md5s.txt,buildids.txt,build.done} and the main stage dir
# $R/ozarchy-bblind-<s7>-stage/main/release/{torus-node (main), bench-throughput (candidate's)}: both arms run the
# candidate's bench-throughput (section 9.1 rule). Idempotent: re-running reuses the worktree and target dir.
set -u
SHA_IN=${1:?usage: ozarchy-bblind-gate2-prep.sh <CANDIDATE_SHA>}
R=/home/oz/bench-results-matched
REPO=/home/oz/projects/torus-economy/Torus-hyperBFT
BASE=ab12c75                                   # origin/perf/item6-phase1 tip before B-blind (cuts 1/2/5/6, detach.sh)
WT_M=/home/oz/projects/wt/main
MAIN_SHA=1cd786d                               # wt/main HEAD; 92a02ed..1cd786d is TESTING.md + .claude/agents only
MAIN_CODE_SHA=92a02ed                          # last main commit with code; the pinned binary is built from it
MAIN_TD=/home/oz/.cargo-target-main
MAIN_NODE_MD5=31a95c654761f582a5c124b4146d6524  # build-id 0c1dc011e5bf9d2071b2073856465cb4f642cfba (section 18)
SEED_TD=/home/oz/.cargo-target-4acdc59          # same flags; reflink copy seeds the incremental build

die() { echo "[$(date +%T)] PREP FAIL: $*" >&2; exit 1; }

git -C "$REPO" fetch -q origin || die "git fetch"
SHA=$(git -C "$REPO" rev-parse --verify -q "$SHA_IN^{commit}") || die "unknown commit $SHA_IN (pushed yet?)"
S7=${SHA:0:7}
git -C "$REPO" merge-base --is-ancestor "$BASE" "$SHA" || die "$S7 does not contain $BASE"
TIP=$(git -C "$REPO" rev-parse origin/perf/item6-phase1)
[ "$TIP" = "$SHA" ] || echo "[$(date +%T)] NOTE: $S7 is not the tip of origin/perf/item6-phase1 (tip ${TIP:0:7})"
echo "[$(date +%T)] candidate $SHA: $(git -C "$REPO" log -1 --format=%s "$SHA")"

WT_C=/home/oz/projects/wt/bblind-$S7
TD_C=/home/oz/.cargo-target-bblind-$S7
B=$R/ozarchy-bblind-$S7-build
ST=$R/ozarchy-bblind-$S7-stage/main/release
mkdir -p "$B" "$ST"

# 1. candidate worktree (detached, clean)
if [ ! -d "$WT_C" ]; then
    git -C "$REPO" worktree add -q --detach "$WT_C" "$SHA" || die "worktree add"
fi
[ "$(git -C "$WT_C" rev-parse HEAD)" = "$SHA" ] || die "$WT_C HEAD != $SHA"
[ -z "$(git -C "$WT_C" status --porcelain)" ] || die "$WT_C is dirty"

# 2. main: worktree at MAIN_SHA, clean, no code change since MAIN_CODE_SHA, binary = pinned md5 and newer than MAIN_CODE_SHA
[ -z "$(git -C "$WT_M" diff --name-only "$MAIN_CODE_SHA" HEAD -- . ':!*.md' ':!.claude' ':!.gitignore')" ] || die "$WT_M has code changes since $MAIN_CODE_SHA"
[ "$(git -C "$WT_M" rev-parse --short=7 HEAD)" = "$MAIN_SHA" ] || die "$WT_M HEAD != $MAIN_SHA"
[ -z "$(git -C "$WT_M" status --porcelain)" ] || die "$WT_M is dirty"
[ "$(md5sum < "$MAIN_TD/release/torus-node" | cut -d' ' -f1)" = "$MAIN_NODE_MD5" ] || die "main torus-node md5 != $MAIN_NODE_MD5"
[ "$(stat -c %Y "$MAIN_TD/release/torus-node")" -ge "$(git -C "$WT_M" log -1 --format=%ct "$MAIN_CODE_SHA")" ] || die "main torus-node older than $MAIN_CODE_SHA"
echo "[$(date +%T)] main torus-node OK: md5 $MAIN_NODE_MD5, $WT_M at $MAIN_SHA clean"

# 3. build the candidate (no other build may run)
[ -z "$(pgrep 'cargo|rustc')" ] || die "another cargo/rustc is running: $(pgrep -a 'cargo|rustc' | head -3)"
[ -e "$TD_C" ] || cp -a --reflink=auto "$SEED_TD" "$TD_C" || die "seed $TD_C"
(
    export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
    export RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"
    cd "$WT_C" || exit 1
    echo "wt=$WT_C head=$(git rev-parse HEAD) td=$TD_C RUSTFLAGS=$RUSTFLAGS DEBUG=$CARGO_PROFILE_RELEASE_DEBUG"
    CARGO_TARGET_DIR=$TD_C cargo build --release -p torus-node || exit 1
    CARGO_TARGET_DIR=$TD_C cargo build --release -p bench-throughput || exit 1
) > "$B/build.log" 2>&1 || { echo "exit=1" > "$B/build.done"; tail -20 "$B/build.log"; die "build (log $B/build.log)"; }
echo "exit=0" > "$B/build.done"
md5sum "$TD_C/release/torus-node" "$TD_C/release/bench-throughput" > "$B/md5s.txt"
for f in "$TD_C/release/torus-node" "$TD_C/release/bench-throughput"; do
    echo "$f $(readelf -n "$f" | awk '/Build ID/{print $3}')"
done > "$B/buildids.txt"
echo "[$(date +%T)] candidate built: $(cut -c1-8 "$B/md5s.txt" | tr '\n' ' ')"

# 4. stage main = main torus-node + candidate bench-throughput
cp -p --reflink=auto "$MAIN_TD/release/torus-node" "$ST/torus-node" || die "stage torus-node"
cp -p --reflink=auto "$TD_C/release/bench-throughput" "$ST/bench-throughput" || die "stage bench"
md5sum "$ST/torus-node" "$ST/bench-throughput" "$MAIN_TD/release/torus-node" > "$R/ozarchy-bblind-$S7-stage/md5s.txt"
[ "$(md5sum < "$ST/torus-node" | cut -d' ' -f1)" = "$MAIN_NODE_MD5" ] || die "staged main node md5"
cmp -s "$ST/bench-throughput" "$TD_C/release/bench-throughput" || die "staged bench differs"
[ "$(md5sum < "$TD_C/release/torus-node" | cut -d' ' -f1)" != "$MAIN_NODE_MD5" ] || die "candidate node == main node?"

# 5. tools drift: the campaign runs run-cell.sh + tools from the candidate worktree, detach.sh from the harness
git -C "$REPO" diff --stat "$BASE" "$SHA" -- tools/matched-bench devnet | tail -5 | sed 's/^/[tools diff vs ab12c75] /'
[ -z "$(pgrep 'cargo|rustc')" ] || die "cargo/rustc still running after build"
echo "[$(date +%T)] PREP OK $S7"
