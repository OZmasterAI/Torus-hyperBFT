#!/usr/bin/env bash
# Runs the 5524646 10-market campaign (no window marker; host verified quiet by the bench-runner) and writes the done marker.
R=/home/oz/bench-results-matched
echo "[$(date +%T)] launch pid=$$"
$R/ozarchy-5524646-10m-campaign.sh
echo "exit=$?" > $R/ozarchy-5524646-10m.campaign.done
