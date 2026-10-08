#!/usr/bin/env bash
timeout 270 bash -c 'until [ -e /home/oz/bench-results-matched/ozarchy-main-prof-10m.campaign.done ]; do sleep 10; done'
echo "rc=$?"; tail -3 /home/oz/bench-results-matched/ozarchy-main-prof-10m.campaign.log
