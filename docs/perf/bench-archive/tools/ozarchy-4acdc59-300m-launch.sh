#!/usr/bin/env bash
# Runs the 4acdc59 300-market campaign and writes the done marker.
R=/home/oz/bench-results-matched
echo "[$(date +%T)] launch pid=$$"
$R/ozarchy-4acdc59-300m-campaign.sh
echo "exit=$?" > $R/ozarchy-4acdc59-300m.campaign.done
