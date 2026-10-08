#!/usr/bin/env bash
# Waits (max 90 min) for the other agent's window marker, then runs the c58775f 10-market campaign.
R=/home/oz/bench-results-matched
S=$EPOCHSECONDS
until [ -e $R/ozarchy-step2-window.done ]; do
    sleep 20
    if [ $((EPOCHSECONDS-S)) -ge 5400 ]; then
        echo "[$(date +%T)] window marker absent after 90 min, not launched"
        echo "exit=window-timeout" > $R/ozarchy-c58775f-10m.campaign.done
        exit 0
    fi
done
echo "[$(date +%T)] window marker seen after $((EPOCHSECONDS-S))s"
$R/ozarchy-c58775f-10m-campaign.sh
echo "exit=$?" > $R/ozarchy-c58775f-10m.campaign.done
