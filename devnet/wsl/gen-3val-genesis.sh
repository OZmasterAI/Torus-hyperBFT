#!/usr/bin/env bash
# gen-3val-genesis.sh — build a bare-metal 3-validator devnet genesis for the
# WSL local devnet (perf/funnel-truth). NO docker.
#
# The genesis = the WEIGHTED testnet genesis with BULK_COUNT (default 100k)
# bench-reproducible funded senders (plus pre-seeded markets, hardhat governance stakers) but with the
# consensus validator set REPLACED by the 3 deterministic devnet validator keys
# (0x01.. / 0x02.. / 0x03..) whose private keys we actually hold, each under a
# test-only EVM address from devnet/wsl/bench-validator-keys.json. permanent_stakes
# are governance-only (CF_STAKING_PERMANENT) and carry NO consensus voting power
# (torus-genesis to_hotstuff_genesis builds the validator set solely from
# .validators), so they are kept untouched.
#
# Output (git-ignored, regenerable — the full genesis is too large for git):
#   devnet/wsl/genesis-3val.json            (override with OUT=/path)
#
# Env:
#   FORCE=1     rebuild the weighted-full genesis even if present.
#   MARKETS=N   emit exactly N markets (default: whatever the weighted base
#               carries — 100 since ad3b8d8). N <= base: keep the first N
#               market rows verbatim (ids 1..N, the bench spreads orders over
#               ids 1..=--markets). N > base: append synthetic S<k>-USD perps
#               following the existing row schema (lot/tick 1.0, im 5.0).
#               The BULK_COUNT funded accounts are untouched either way.
#   OUT=/path   where to write the 3-val genesis.
#   BULK_COUNT=N  funded bulk senders "bulk-test 60".."bulk-test 60+N-1" in the
#               weighted-full genesis (testnet/gen-weighted-genesis.sh; default
#               100000, unchanged). An EXISTING weighted-full genesis is reused
#               only if it was built with the same N (its params sidecar
#               genesis-weighted-full.params.json, else a count of its
#               "bulk-test N" rows); a different N is FATAL: rebuild with
#               FORCE=1. Never rebuilt implicitly (1M accounts take minutes and
#               ~290 MB). Cost at N = 1M: each jq pass over the genesis parses
#               ~240-290 MB (~3 s, a rewrite ~7 s and ~1.4 GB RSS with jq 1.8);
#               this script does two rewrites (steps 3 and 5, more with the optional
#               steps 4 and 6) and one summary pass.
#   EVM_BLOCK_INTERVAL=N  genesis EVM cadence N (.evm.block_interval: EVM txs
#               only at heights with height % N == 0; bench Q12). Integer in
#               2..=100 (torus_types::EVM_BLOCK_INTERVAL_RANGE, what governance
#               can set), else FATAL before anything is written. Patches only
#               OUT, never the reused weighted-full genesis. Unset or empty =
#               the field stays absent (node default 11) and OUT is
#               byte-identical to a run without the knob.
#   BOOK_MODE=<mode>  the order-book storage mode, genesis field
#               .consensus.book_mode (owner s131: chain-wide, the node takes
#               its mode from genesis and refuses a TORUS_BOOK_ROWS that
#               disagrees). classic | order_rows | level_authority |
#               level_authority_chunked, or 0-3 (written as the name); else
#               FATAL before anything is written. Patches only OUT, never the
#               reused weighted-full genesis. Unset, empty, classic or 0 = the
#               field stays absent (absent is Classic, so one Classic chain has
#               one genesis) and OUT is byte-identical to a run without it.
#   VALIDATOR_STAKES=a,b,c  per-validator stake WEIGHTS (val0,val1,val2;
#               positive integers): stake_i = weight_i x the devnet base stake
#               (devnet/genesis.json, 1e24 wei = 1M TRS), so consensus power and
#               the state hash attestation weight follow a:b:c. For the
#               one-validator-down baseline (tools/matched-bench/run-cell.sh
#               KILL_NO_RESTART): 2,2,1 and kill val2 leaves 4/5 > 2/3 of the
#               power up; with equal stakes a dead validator halts the chain.
#               Patches only OUT (the validator set this script writes). Unset or
#               empty = the base stakes and OUT is byte-identical to a run
#               without the knob.
#   N_VALIDATORS=3|4  the validator count (default 3). 4 = the dead-leader
#               bench 9p devnet (owner s138): devnet/genesis.json's first 4
#               validators (key 04's pubkey is the 4th) with the addresses of
#               devnet/wsl/keys-4val/bench-validator-keys.json (indices 0..2 =
#               bench-validator-keys.json, 3 = mnemonic index 106). 3 (or unset)
#               = OUT byte-identical to a run without the knob.
#   VALIDATOR_STAKES with N_VALIDATORS=4 takes four weights a,b,c,d.
#   DL_K=K  DL_JCAP=J  dead-leader skip genesis parameters (branch
#               feat/dead-leader-p3, docs/plans/dead-leader-skip.md):
#               .consensus.leader_miss_threshold = K (0 = off, else 2..=64;
#               node default 0 = off, owner s138) and .consensus.leader_exclusion_cap = J
#               (0..=10; node default 6). Each written ONLY when given (unset
#               or empty = the field stays absent, OUT byte-identical). A
#               binary without the fields ignores them (the genesis loader
#               has no deny_unknown_fields): run-cell.sh refuses that.
#   BENCH_BIN=  bench-throughput binary (default: cargo-bin resolution, which
#               honours CARGO_TARGET_DIR, then <repo>/target/release).
set -euo pipefail
cd "$(dirname "$0")/../.."
REPO=$(pwd)

. "$REPO/testnet/lib/cargo-bin.sh"
BIN="${BENCH_BIN:-$(cargo_bin bench-throughput "$REPO")}" || exit 1
FULL="$REPO/testnet/genesis-weighted-full.json"
OUT="${OUT:-$REPO/devnet/wsl/genesis-3val.json}"

[ -x "$BIN" ] || { echo "FATAL: missing $BIN — build first: cargo build --release -p bench-throughput" >&2; exit 1; }
command -v jq >/dev/null || { echo "FATAL: jq not installed" >&2; exit 1; }
if [ -n "${EVM_BLOCK_INTERVAL:-}" ]; then
    [[ "$EVM_BLOCK_INTERVAL" =~ ^[1-9][0-9]{0,2}$ ]] && [ "$EVM_BLOCK_INTERVAL" -ge 2 ] && [ "$EVM_BLOCK_INTERVAL" -le 100 ] \
        || { echo "FATAL: EVM_BLOCK_INTERVAL must be an integer in 2..=100 (torus_types::EVM_BLOCK_INTERVAL_RANGE) (got '$EVM_BLOCK_INTERVAL')" >&2; exit 1; }
fi
# BOOK_MODE -> the genesis name (torus_types::book_mode::BookMode::name);
# empty for Classic (the field stays absent).
BOOK_MODE_NAME=""
if [ -n "${BOOK_MODE:-}" ]; then
    case "$BOOK_MODE" in
        0|classic) BOOK_MODE_NAME="" ;;  # absent = Classic: never written explicitly
        1|order_rows) BOOK_MODE_NAME=order_rows ;;
        2|level_authority) BOOK_MODE_NAME=level_authority ;;
        3|level_authority_chunked) BOOK_MODE_NAME=level_authority_chunked ;;
        *) echo "FATAL: BOOK_MODE must be classic, order_rows, level_authority, level_authority_chunked or 0-3 (got '$BOOK_MODE')" >&2; exit 1 ;;
    esac
fi

N_VALIDATORS="${N_VALIDATORS:-3}"
case "$N_VALIDATORS" in
    3|4) ;;
    *) echo "FATAL: N_VALIDATORS must be 3 or 4 (got '$N_VALIDATORS')" >&2; exit 1 ;;
esac
if [ -n "${VALIDATOR_STAKES:-}" ]; then
    if [ "$N_VALIDATORS" = 4 ]; then
        [[ "$VALIDATOR_STAKES" =~ ^[1-9][0-9]{0,5},[1-9][0-9]{0,5},[1-9][0-9]{0,5},[1-9][0-9]{0,5}$ ]] \
            || { echo "FATAL: VALIDATOR_STAKES must be four positive integer weights a,b,c,d with N_VALIDATORS=4 (got '$VALIDATOR_STAKES')" >&2; exit 1; }
    else
        [[ "$VALIDATOR_STAKES" =~ ^[1-9][0-9]{0,5},[1-9][0-9]{0,5},[1-9][0-9]{0,5}$ ]] \
            || { echo "FATAL: VALIDATOR_STAKES must be three positive integer weights a,b,c (got '$VALIDATOR_STAKES')" >&2; exit 1; }
    fi
fi
# Dead-leader skip K / j cap (torus_types::DeadLeaderParams::check ranges).
if [ -n "${DL_K:-}" ]; then
    [[ "$DL_K" =~ ^(0|[1-9][0-9]?)$ ]] && { [ "$DL_K" = 0 ] || { [ "$DL_K" -ge 2 ] && [ "$DL_K" -le 64 ]; }; } \
        || { echo "FATAL: DL_K (consensus.leader_miss_threshold) must be 0 (off) or 2..=64 (got '$DL_K')" >&2; exit 1; }
fi
if [ -n "${DL_JCAP:-}" ]; then
    [[ "$DL_JCAP" =~ ^([0-9]|10)$ ]] \
        || { echo "FATAL: DL_JCAP (consensus.leader_exclusion_cap) must be 0..=10 (got '$DL_JCAP')" >&2; exit 1; }
fi

BULK_COUNT="${BULK_COUNT:-100000}"
[[ "$BULK_COUNT" =~ ^[1-9][0-9]*$ ]] || { echo "FATAL: BULK_COUNT must be a positive integer (got '$BULK_COUNT')" >&2; exit 1; }
FULL_PARAMS="${FULL%.json}.params.json"

# full_bulk_count: the BULK_COUNT that built $FULL. The params sidecar
# gen-weighted-genesis.sh writes, when its recorded size is FULL's (else it
# describes another file); otherwise (a genesis built before the sidecar) the
# number of "bulk-test N" native rows, one jq pass.
full_bulk_count() {
    local n bytes
    if [ -f "$FULL_PARAMS" ]; then
        bytes=$(wc -c < "$FULL")
        n=$(jq -r --argjson b "$bytes" 'select(.bytes == $b) | .bulk_count' "$FULL_PARAMS" 2>/dev/null) || n=""
        [[ "$n" =~ ^[0-9]+$ ]] && { echo "$n"; return 0; }
    fi
    jq '[.native_balances[]? | (.note // "") | select(test("^bulk-test [0-9]+$"))] | length' "$FULL"
}

# 1. Materialise the weighted-full genesis (adds BULK_COUNT bulk senders).
if [ "${FORCE:-0}" = 1 ] || [ ! -f "$FULL" ]; then
    echo "building weighted-full genesis (BULK_COUNT=$BULK_COUNT bulk senders)..."
    # Parent OUT names the final 3-validator genesis, not this child artifact.
    OUT="$FULL" BIN="$BIN" BULK_COUNT="$BULK_COUNT" "$REPO/testnet/gen-weighted-genesis.sh"
else
    HAVE_BULK=$(full_bulk_count) || { echo "FATAL: cannot read $FULL to check its BULK_COUNT; rebuild with FORCE=1" >&2; exit 1; }
    [ "$HAVE_BULK" = "$BULK_COUNT" ] || {
        echo "FATAL: $FULL was built with BULK_COUNT=$HAVE_BULK, this run wants BULK_COUNT=$BULK_COUNT;" \
            "rebuild it with FORCE=1 (never reused silently)" >&2
        exit 1
    }
    echo "reusing existing $FULL (BULK_COUNT=$HAVE_BULK)"
fi

# 2. The 3 devnet validators (pubkeys == ed25519 pubkeys of keys 01/02/03),
#    lifted from devnet/genesis.json so pubkey/stake/commission match, with the
#    made-up EVM addresses (0x1000..01/0x2000..02/0x3000..03) swapped for the
#    test-only addresses in bench-validator-keys.json, whose private keys we
#    hold, so each validator can sign oracle price submissions. Consensus uses
#    the ed25519 pubkeys only; the address is the validator's account identity.
KEYS="$REPO/devnet/wsl/bench-validator-keys.json"
[ "$N_VALIDATORS" = 4 ] && KEYS="$REPO/devnet/wsl/keys-4val/bench-validator-keys.json"
[ -f "$KEYS" ] || { echo "FATAL: missing $KEYS" >&2; exit 1; }
VALS=$(jq --slurpfile k "$KEYS" --argjson n "$N_VALIDATORS" '
    if (.validators | length) < $n then error("devnet/genesis.json has \(.validators | length) validators, need \($n)") else . end
    | [.validators[0:$n] | to_entries[] | .key as $i
        | ([$k[0].validators[] | select(.index == $i) | .address]) as $a
        | if ($a|length) != 1 then error("bench-validator-keys.json: need exactly one entry with index \($i)")
          else .value | .address = ($a[0] | ascii_downcase) end]' "$REPO/devnet/genesis.json")
[ "$(jq '[.[].address | select(test("^0x[0-9a-f]{40}$"))] | unique | length' <<<"$VALS")" = "$N_VALIDATORS" ] \
    || { echo "FATAL: $(basename "$KEYS") must give $N_VALIDATORS distinct 20-byte addresses" >&2; exit 1; }
# 2b. Optional stake weights: stake = weight x the base stake, as decimal
#     strings (a 1 followed by zeros, so the product is the weight followed by
#     those zeros; jq numbers would lose the 25 digits).
if [ -n "${VALIDATOR_STAKES:-}" ]; then
    VALS=$(jq --arg w "$VALIDATOR_STAKES" '($w | split(",")) as $w
        | [to_entries[] | .key as $i | .value
            | if (.stake | test("^10+$")) then .stake = ($w[$i] + .stake[1:])
              else error("devnet/genesis.json validator \($i) stake \(.stake) is not 1 followed by zeros") end]' <<<"$VALS") \
        || { echo "FATAL: VALIDATOR_STAKES=$VALIDATOR_STAKES: stake patch failed" >&2; exit 1; }
fi

# 3. Weighted accounts + 3-val consensus set (+ optional market count).
if [ -n "${MARKETS:-}" ]; then
    [[ "$MARKETS" =~ ^[0-9]+$ ]] && [ "$MARKETS" -ge 1 ] \
        || { echo "FATAL: MARKETS must be a positive integer (got '$MARKETS')" >&2; exit 1; }
    # Keep the first N rows verbatim; synthesise S<k>-USD rows past the base.
    jq --argjson vals "$VALS" --argjson n "$MARKETS" '
        .validators = $vals
        | .markets = (.markets | sort_by(.market_id))
        | .markets = (
            if ($n <= (.markets|length)) then .markets[0:$n]
            else .markets + [ range((.markets|length)+1; $n+1) | {
                market_id: .,
                base_asset: ("S" + tostring),
                quote_asset: "USD",
                lot_size: "1.0",
                tick_size: "1.0",
                initial_margin: "5.0",
                note: ("synthetic bench market S" + tostring + "-USD (added for multi-market throughput sweeps)")
            } ] end )' "$FULL" > "$OUT"
    # Sanity: exactly N rows, ids 1..N contiguous, all rows have the schema.
    got=$(jq '.markets|length' "$OUT")
    ids_ok=$(jq '[.markets[].market_id] == [range(1; (.markets|length)+1)]' "$OUT")
    [ "$got" = "$MARKETS" ] && [ "$ids_ok" = true ] \
        || { echo "FATAL: market synthesis failed (got=$got ids_contiguous=$ids_ok)" >&2; exit 1; }
else
    jq --argjson vals "$VALS" '.validators = $vals' "$FULL" > "$OUT"
fi

# 4. Optional pacemaker base timeout override (s55 timeout-cliff A/B). Genesis
#    field, so all three validators agree; default untouched (500 ms).
if [ -n "${TIMEOUT_BASE_MS:-}" ]; then
    [[ "$TIMEOUT_BASE_MS" =~ ^[0-9]+$ ]] && [ "$TIMEOUT_BASE_MS" -ge 100 ] \
        || { echo "FATAL: TIMEOUT_BASE_MS must be an integer >= 100 (got '$TIMEOUT_BASE_MS')" >&2; exit 1; }
    tmp=$(mktemp) && jq --argjson t "$TIMEOUT_BASE_MS" '.consensus.timeout_base_ms = $t' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi

# 4b. Optional genesis EVM cadence N (bench Q12; EvmConfig.block_interval,
#     absent = 11). Validated at the top; chain-wide, so all validators agree.
if [ -n "${EVM_BLOCK_INTERVAL:-}" ]; then
    tmp=$(mktemp) && jq --argjson n "$EVM_BLOCK_INTERVAL" '.evm.block_interval = $n' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi

# 4c. Optional order-book storage mode (owner s131). Validated at the top;
#     chain-wide, so all validators agree (absent = classic).
if [ -n "$BOOK_MODE_NAME" ]; then
    tmp=$(mktemp) && jq --arg m "$BOOK_MODE_NAME" '.consensus.book_mode = $m' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi

# 5. Running state hash activation (docs/plans/running-state-hash-impl.md).
#    CHAIN-WIDE: every node hashes from this height. Owner s136: ON in every
#    genesis this script writes, named EXPLICITLY: the node reads a genesis
#    naming no activation as off (its legacy meaning; Codex P1: an existing
#    file's digest binds data dirs, so the parser never reinterprets it).
#    unset = consensus.activations = {running_state_hash: 1, book_window_commit: 1}
#            (torus_types::DEFAULT_GENESIS_ACTIVATIONS; book_window_commit, L6.1,
#            only for a Classic book mode and unless BOOK_WINDOW_COMMIT=0);
#    H     = from H (the alias field state_hash_activation_height, as before);
#    off   = no activation written (the pre-s136 bytes).
#    Whatever the base carries for these (the alias, the L6.1 bool,
#    activations.running_state_hash / book_window_commit) is dropped first, so
#    this script alone decides (Codex P2: an inherited alias survived 'off' while
#    the summary said off). One jq pass for all three.
if [ -n "${STATE_HASH_ACTIVATION:-}" ] && [ "$STATE_HASH_ACTIVATION" != off ]; then
    [[ "$STATE_HASH_ACTIVATION" =~ ^[0-9]+$ ]] && [ "$STATE_HASH_ACTIVATION" -ge 1 ] \
        || { echo "FATAL: STATE_HASH_ACTIVATION must be an integer >= 1 or 'off' (got '$STATE_HASH_ACTIVATION')" >&2; exit 1; }
fi
win=true; [ "${BOOK_WINDOW_COMMIT:-}" = 0 ] && win=false
tmp=$(mktemp) && jq --arg sha "${STATE_HASH_ACTIVATION:-}" --argjson win "$win" '
    .consensus |= (del(.state_hash_activation_height, .book_window_commit)
        | if has("activations") then
              .activations |= del(.running_state_hash, .book_window_commit)
              | if .activations == {} then del(.activations) else . end
          else . end)
    | ((.consensus.book_mode // "classic") | tostring | ascii_downcase) as $m
    | if $sha == "off" then .
      elif $sha != "" then .consensus.state_hash_activation_height = ($sha | tonumber)
      else .consensus.activations = ((.consensus.activations // {}) + {running_state_hash: 1}
          + (if $win and ($m == "classic" or $m == "0") then {book_window_commit: 1} else {} end))
      end' "$OUT" > "$tmp" && mv "$tmp" "$OUT"

# 5b. Optional dead-leader skip K / j cap (feat/dead-leader-p3). CHAIN-WIDE;
#     validated at the top; written only when given (absent = node defaults 3 / 6).
if [ -n "${DL_K:-}" ]; then
    tmp=$(mktemp) && jq --argjson k "$DL_K" '.consensus.leader_miss_threshold = $k' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi
if [ -n "${DL_JCAP:-}" ]; then
    tmp=$(mktemp) && jq --argjson j "$DL_JCAP" '.consensus.leader_exclusion_cap = $j' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi

# 6. Optional L6.1 window-committed Classic books (docs/plans/l61-window-committed-books.md).
#    CHAIN-WIDE genesis field (alias of activations.book_window_commit at the running
#    hash's height); needs the running hash (the node refuses it otherwise).
if [ "${BOOK_WINDOW_COMMIT:-0}" = 1 ]; then
    [ "${STATE_HASH_ACTIVATION:-}" != off ] \
        || { echo "FATAL: BOOK_WINDOW_COMMIT=1 needs the running state hash (STATE_HASH_ACTIVATION=off)" >&2; exit 1; }
    tmp=$(mktemp) && jq '.consensus.book_window_commit = true' "$OUT" > "$tmp" && mv "$tmp" "$OUT"
fi

echo "wrote $OUT"
# One jq pass for the whole summary (each pass parses the full genesis, ~3 s at 1M accounts).
jq -r '
    "  timeout_ms = \(.consensus.timeout_base_ms)",
    "  evm_block_interval = \(.evm.block_interval // "absent (node default 11)")",
    "  state_hash_activation = \(.consensus.activations.running_state_hash // .consensus.state_hash_activation_height // "off")",
    (if (.consensus.activations.book_window_commit // null) != null or .consensus.book_window_commit == true
     then "  book_window_commit = on (L6.1)" else empty end),
    "  book_mode  = \(.consensus.book_mode // "absent (classic)")",
    (if .consensus.leader_miss_threshold != null or .consensus.leader_exclusion_cap != null
     then "  dead_leader = leader_miss_threshold \(.consensus.leader_miss_threshold // "absent (node default 0 = off)") leader_exclusion_cap \(.consensus.leader_exclusion_cap // "absent (node default 6)")"
     else empty end),
    "  chain_id   = \(.chain_id)",
    "  validators = \(.validators|length)  (\([.validators[].address] | join(" ")) )",
    "  native_bal = \(.native_balances|length)",
    "  bulk_count = \([.native_balances[]? | (.note // "") | select(test("^bulk-test [0-9]+$"))] | length)",
    "  accounts   = \(.accounts|length)",
    "  markets    = \(.markets|length)",
    "  perm_stakes= \(.permanent_stakes|length)"' "$OUT"
if [ -n "${VALIDATOR_STAKES:-}" ]; then
    echo "  validator_stakes = $(jq -r '[.[].stake] | join(" ")' <<<"$VALS") (VALIDATOR_STAKES=$VALIDATOR_STAKES)"
fi
