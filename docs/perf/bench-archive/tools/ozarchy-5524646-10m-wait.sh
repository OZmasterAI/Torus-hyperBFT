#!/usr/bin/env bash
# Blocks until the campaign done marker exists, then prints it and the log tail.
R=/home/oz/bench-results-matched
until [ -e $R/ozarchy-5524646-10m.campaign.done ]; do sleep 30; done
cat $R/ozarchy-5524646-10m.campaign.done; tail -20 $R/ozarchy-5524646-10m.campaign.log
