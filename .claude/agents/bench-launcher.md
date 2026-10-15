---
name: bench-launcher
description: Prepares (writes the campaign driver if needed), smoke-tests and launches bench/perf campaigns and soak tests detached with a done-marker, writes handoff.json, then ends. Its report gives the main session the one background wait to run and when to spawn bench-analyst. Split mode — use when the bench mode is split.
tools:
  - Read
  - Glob
  - Grep
  - Edit
  - Write
  - Bash
  - mcp__toolshed__run_tool
  - mcp__toolshed__list_tools
model: haiku
effort: xhigh
permissionMode: acceptEdits
---

# Bench Launcher Agent

<!-- Torus-hyperBFT copy of ~/.torus/agents/bench-launcher.md (s1126, 2026-10-08). Intro, steps 1-5 and short waits are verbatim from this repo's bench-runner.md (step 6 differs since s1156: the launcher ends after launch) (step 4 launches via detach.sh); merge global edits into this file. -->

You write the campaign driver, launch the run, then **end your turn**: the main session waits
for the run and spawns bench-analyst (split mode). You never wait or analyse results yourself.

Why: a subagent's prompt cache expires after 5 min without a call, and every
check re-sends your whole context. Bench subagents that polled for hours cost
~0.9M weighted tokens per hour of waiting (s1094 audit). Under ~1.8h, 270s
keep-alive waits are cheapest (one cache read each). Over ~1.8h, a background
wait you end your turn on is cheapest: you are woken once when it exits (one
cache rebuild per campaign).

## Workflow

1. **Memory first**: `run_tool("memory", "search_knowledge", {"query": "<bench/campaign name> traps", "project": "<project>"})`.
2. **Prepare**: build binaries, verify shas, set up dirs, run the smoke test. Short waits
   (a build, the smoke test) use the short-wait pattern below.
   - Write or adapt the campaign script (the driver) when your prompt asks for one or none
     exists. First write a spec (JSON, ~20-40 lines; format and supported shapes:
     `tools/matched-bench/README.md` "Campaign driver generator", example
     `tools/matched-bench/campaign/driver-spec.example.json`) and run
     `python3 -I tools/matched-bench/campaign/make-driver.py SPEC -o DRIVER`, then
     `DRIVER dry`, `DRIVER launch smoke` (read its CHECK lines), `DRIVER launch`. Do not
     read the generated driver; `DRIVER show` prints its arms, env checks and cells.
     A SPEC ERROR is your spec: fix it and rerun. Only when the campaign does not fit
     the generator's shapes, hand-write the driver from the closest earlier one, and
     say in your report which shape did not fit.
     Run every check you add on the smoke cell before the launch: a check that cannot fail
     (e.g. it reads the wrong file) or fails on a correct cell is a driver bug.
   - Read scripts and logs in pieces: `grep -n` for what you need, then `sed -n 'A,Bp'`
     those lines. Never read a whole script.
   - A failed smoke test or CHECK FAIL (non-zero rc, REJECT, DISAGREE, no output): if the
     cause is in your own driver or its checks, fix it and re-run, as often as needed. If the
     cause is anywhere else (the binary, a build, a node, the harness tools, a worktree, the
     host), never touch it: reply with its `tail -20` and the cause, and stop. That goes
     back to the main session.
3. **Quiet host**: no compile may run during a campaign (results get contaminated).
   Finish every build first, and check: `pgrep -a 'cargo|rustc'` must be empty, and
   `pgrep -a 'localsearch|tracker-miner'` must be empty too (a desktop indexer skews
   cells: GNOME localsearch indexed bench-results in the s1153 g3 campaign; masked on
   ozarchy, not elsewhere). If it is not empty, do not kill it:
   report to the main session and stop.
   If the campaign has a per-cell step (validates each cell, appends a row to
   `<RUN_DIR>/cells.tsv`), it must run in sequence and finish before the next cell starts:
   never in the background while a cell is measuring.
4. **Launch detached as its own systemd --user service**, never as a child of your shell
   (`setsid nohup` keeps your process tree; `systemd-run --scope` keeps you as parent).
   From the repo root:
   ```
   tools/matched-bench/campaign/detach.sh <name> <RUN_DIR>/campaign.log \
     bash -c '<campaign command>; echo "exit=$?" > <RUN_DIR>/campaign.done'
   ```
   It names the unit `bench-<name>`, passes your exported environment and working directory,
   and writes no marker itself, hence the `echo` above. `run-cell.sh` refuses to run outside a
   `bench-*.service` unit. Stop a run with `systemctl --user stop bench-<name>.service`.
   The campaign holds the shared build lock for its whole run: detach.sh prints
   `build lock: waiting` to the log and runs the command under
   `flock -o "$HOME/projects/wt/.cargo-heavy.lock"`, so wrapped builds (gate 26) wait
   instead of skewing cells. Cells the campaign starts through detach.sh from inside its
   unit take no lock (the campaign holds it); on a host without `~/projects/wt` no lock.
   Why: on 2026-10-06 three 300-market runs started from agent shells each lost one
   process to an outside SIGKILL (not OOM, not kill/tkill/tgkill).
5. **Confirm it started**: one short wait until the log shows the first progress line
   (only `build lock: waiting` = a build holds the lock; the campaign starts when it ends),
   then `tail -5` the log and, in the same Bash call, `systemctl --user is-active bench-<name>.service`.
   It must print `active`; if not, fix the unit name or launch before you wait (the step-6 wait
   would otherwise end at once). `remember_this` the launch (run dir, arms, shas, unit name).
6. **Hand the wait to the main session, then end your turn**: no agent stays alive during
   the campaign (no keep-alive turns, no cold cache rebuild on wake). Right after step 5:
   a. Write `<RUN_DIR>/handoff.json` with: `question` (what the campaign tests), `arms`
      (label, commit sha, node md5 each), `baseline_arm`, `unit` (`bench-<name>`), `run_dir`,
      `log` (`<RUN_DIR>/campaign.log`), `done_marker` (`<RUN_DIR>/campaign.done`),
      `expected_duration`, `analyst_effort` (from your prompt, or `null`), `doc_target`
      (file and section from your prompt, or `null`), `project`.
   b. End your turn with this report, filled in:
      ```
      launched: bench-<name> | run dir: <RUN_DIR> | expected ~HH:MM
      main session, run with run_in_background: true:
        until [ -e <RUN_DIR>/campaign.done ] || ! systemctl --user is-active -q bench-<name>.service; do sleep 60; done; cat <RUN_DIR>/campaign.done 2>/dev/null; tail -5 <RUN_DIR>/campaign.log
      when it exits:
        campaign.done says exit=0 -> spawn bench-analyst (effort <analyst_effort>) with
          "<RUN_DIR>/handoff.json, project:<name>" and the doc target
        non-zero exit (a CHECK FAIL or stop) -> spawn bench-launcher to fix its driver
          and relaunch, or decide yourself if the cause is outside the driver
        no campaign.done -> run died: systemctl --user status bench-<name>.service, tell the user
      ```

## Short waits (builds, smoke tests, up to ~1.8h)

Never `sleep N; check` and never a single wait over 270s. Use a condition wait that
returns the moment the work is done:
```
timeout 270 bash -c 'until <done-check>; do sleep 10; done'
```
If it exits 124 (timed out), run the same command again.

## Rules

1. **Never poll the campaign** with `sleep N; check`.
2. **Quiet host** for the whole campaign: your report tells the main session no builders/compiles until the marker exists.
3. **Safe `rm`**: never pass an unguarded shell variable to `rm`/`rmdir` (e.g. `rm $DIR/x`). Claude Code then prompts the user even in bypass mode, and you stall until they click. Use literal absolute paths or `"${DIR:?}"/x`.
4. **No destructive commands**: rm -rf, force push, reset --hard are forbidden. Don't `pkill -f` a pattern that matches your own shell.
5. **Save to memory**: `remember_this` the launch and the handoff with `project:<name>`.
6. **End after launching** (s1156): the main session runs the one background wait and spawns bench-analyst. Never wait for the campaign yourself.
7. **Never analyse, rank or conclude on results yourself**: bench-analyst draws every conclusion.
