#!/bin/bash
# S405 mesh-watchdog Task 5: induce the fast-restart gossipsub subscription
# wedge on the docker devnet and verify the watchdog heals it.
#
# The footgun being reproduced: an in-place restart (docker restart -t 0)
# races the dying QUIC connection; the fresh connection's subscription
# exchange can be lost, leaving the peer connected-but-unsubscribed on the
# observer (S395). Base builds stay degraded indefinitely; the watchdog build
# must detect (subscribed_validators < expected) and heal (forced reconnect)
# within grace (60s) + one mesh tick (10s) + reconnect slack.
#
# Usage: bash devnet/scripts/wedge-repro-s405.sh [attempts]
# Requires: devnet up (docker compose -f devnet/docker-compose.yml up -d) with
# an image that exposes the S405 mesh metrics on validator-0 (host port 9091).
set -u
cd "$(dirname "$0")/../.."
OUT=devnet/wedge-repro-s405
mkdir -p "$OUT"
LOG="$OUT/run-$(date -u +%Y%m%dT%H%M%S).log"
M="http://127.0.0.1:9091/metrics"
ATTEMPTS="${1:-10}"
EXPECTED_SUBSCRIBED=3   # validator-0 sees validators 1..3

log() { echo "[$(date -u +%H:%M:%S)] $*" | tee -a "$LOG"; }
metric() { curl -s "$M" | awk -v m="$1" '$1 == m {print $2; exit}'; }
views_and_height() { echo "$(metric torus_consensus_view) $(metric torus_block_height)"; }

TARGET=$(docker compose -f devnet/docker-compose.yml ps -q validator-1)
if [ -z "$TARGET" ]; then log "FATAL: validator-1 container not found"; exit 1; fi

log "=== wedge repro: $ATTEMPTS in-place restarts of validator-1, observer=validator-0 ==="
log "binary: $(docker compose -f devnet/docker-compose.yml images validator-0 | tail -1)"

# Base mode (control run): pre-watchdog binaries lack the subscription metric,
# so wedge detection falls back to sustained views/block degradation — the
# S395 signature was ~1.4 views/block persisting indefinitely; healthy is ~1.0.
MODE=fixed
if [ -z "$(metric torus_consensus_subscribed_validators)" ]; then
    MODE=base
    log "subscription metric absent -> BASE mode (views/block detection)"
fi

# Wait for a healthy mesh before starting.
if [ "$MODE" = fixed ]; then
    for _ in $(seq 1 30); do
        SUB=$(metric torus_consensus_subscribed_validators)
        [ "${SUB:-0}" -ge "$EXPECTED_SUBSCRIBED" ] && break
        sleep 5
    done
    SUB=$(metric torus_consensus_subscribed_validators)
    log "pre-flight: subscribed_validators=$SUB (need $EXPECTED_SUBSCRIBED)"
    [ "${SUB:-0}" -ge "$EXPECTED_SUBSCRIBED" ] || { log "FATAL: mesh never became healthy"; exit 1; }
else
    for _ in $(seq 1 30); do
        H=$(metric torus_block_height)
        [ "${H:-0}" -gt 10 ] && break
        sleep 5
    done
    log "pre-flight (base): height=$(metric torus_block_height)"
fi

# One 30s views/block window (base-mode wedge detector). Prints 99 on stall.
vpb_window() {
    read -r V0 H0 <<<"$(views_and_height)"
    sleep 30
    read -r V1 H1 <<<"$(views_and_height)"
    python3 -c "dv=$V1-$V0; dh=$H1-$H0; print(f'{dv/dh:.2f}' if dh>0 else '99')"
}

WEDGES=0
HEALS=0
for i in $(seq 1 "$ATTEMPTS"); do
    read -r V0 H0 <<<"$(views_and_height)"
    log "attempt $i/$ATTEMPTS: docker restart -t0 validator-1"
    docker restart -t 0 "$TARGET" >/dev/null
    sleep 25   # reconnect + normal subscription exchange comfortably done by now
    if [ "$MODE" = fixed ]; then
        SUB=$(metric torus_consensus_subscribed_validators)
        KICKS=$(metric torus_mesh_watchdog_disconnects_total)
        read -r V1 H1 <<<"$(views_and_height)"
        VPB=$(python3 -c "dv=$V1-$V0; dh=$H1-$H0; print(f'{dv/dh:.2f}' if dh>0 else 'stalled')")
        log "  t+25s subscribed=$SUB kicks=${KICKS:-0} views/block=$VPB"
        if [ "${SUB:-0}" -lt "$EXPECTED_SUBSCRIBED" ]; then
            WEDGES=$((WEDGES + 1))
            log "  WEDGE INDUCED (subscribed=$SUB) — watching for watchdog heal"
            K0=${KICKS:-0}
            HEALED=""
            for t in $(seq 1 15); do   # up to 150s: 60s grace + tick + reconnect slack
                sleep 10
                SUB=$(metric torus_consensus_subscribed_validators)
                K1=$(metric torus_mesh_watchdog_disconnects_total)
                log "  t+$((25 + t * 10))s subscribed=$SUB kicks=${K1:-0}"
                if [ "${SUB:-0}" -ge "$EXPECTED_SUBSCRIBED" ]; then
                    HEALS=$((HEALS + 1))
                    HEALED=yes
                    log "  HEALED after $((25 + t * 10))s total (watchdog kicks: $((${K1:-0} - K0)))"
                    break
                fi
            done
            [ -n "$HEALED" ] || log "  NOT HEALED within 175s — watchdog FAILED (or base build)"
            sleep 20   # settle before next attempt
        fi
    else
        # Base mode: wedge = views/block > 1.6 across three consecutive 30s
        # windows (transient restart churn recovers by then; S395 doesn't).
        W1=$(vpb_window); W2=$(vpb_window); W3=$(vpb_window)
        log "  base windows: $W1 / $W2 / $W3 views/block"
        WEDGED=$(python3 -c "print(1 if min($W1,$W2,$W3) > 1.6 else 0)")
        if [ "$WEDGED" = 1 ]; then
            WEDGES=$((WEDGES + 1))
            log "  WEDGE INDUCED (base) — watching 90s for spontaneous heal (expected: none)"
            W4=$(vpb_window); W5=$(vpb_window); W6=$(vpb_window)
            log "  base heal-watch: $W4 / $W5 / $W6 views/block"
            if python3 -c "exit(0 if max($W4,$W5,$W6) < 1.3 else 1)"; then
                HEALS=$((HEALS + 1))
                log "  spontaneously healed (unexpected on base)"
            else
                log "  STILL DEGRADED after 3 more windows — S395 behavior confirmed on base"
            fi
        fi
    fi
done

log "=== summary: attempts=$ATTEMPTS wedges_induced=$WEDGES healed=$HEALS ==="
log "log: $LOG"
