#!/usr/bin/env bash
# Runs the main 300-market host-check campaign and writes the done marker.
R=/home/oz/bench-results-matched
echo "[$(date +%T)] launch pid=$$"
$R/ozarchy-main-300m-hostcheck-campaign.sh
echo "exit=$?" > $R/ozarchy-main-300m-hostcheck.campaign.done
