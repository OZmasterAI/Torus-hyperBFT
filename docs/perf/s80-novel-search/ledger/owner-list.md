# Owner's s136 list (EXCLUDED from the s80 novel search: being built by another machine or already decided)

## Roadmap table
- TC-required (option B + tip signing): liveness fix, phase D one-down bench
- Dead-leader skip (b1): phases 1-4 (K=3, j cap 6), one-fresh-proposer-per-view rule
- Batch 10: L6.1 + L2.1 + running state hash on, fail-stop default on, genesis-default fix
- 9k one-down baseline (digest wait, counter validation, cancel-all backlog check)
- Testnet genesis file + checklist
- Jailing design (J0-J4, double-sign evidence E1/E2, gate JailVote)
- Roadmap + tracker refresh
- Trial net (archy + cachyoz + 18c)
- WAN items: PE (pipelined execution over WAN), push-manifest threshold, L4.1-L4.4, archive-node choice
- T3.1 erasure coding follow-ups (+ RH4 shard-count fix); shard custody off
- Item 7 follow-ups: option S (closed, superseded by option 0 = EVM fast path), next EVM-lanes phase "T", C0 parked
- 9l binary-gap check (53a6d83c vs 29bfc705)
- 9m item 6 Phase 5 exec profile
- 9n 8f deep-book re-run
- T5.3 p99 attribution; A1/Q6 checkpoint slicing parked behind it
- Codex reviews of batch 10 / TC; rebasing dead-leader onto TC; devnet wipe; campaign specs for new defaults

## Owner decisions (not ours)
- 3 or 4 validators; liquidator-vault seed amount (seed capital in genesis + ADL); archive node; PE over WAN; push-manifest threshold; M2 timestamp lower bound; jailing timing; bench unpause.
- Decided: book mode Classic, running hash on, L6.1/L2.1/fail-stop on, one-fresh-proposer-per-view, dead-leader K=3 / j cap 6.

## After testnet, before mainnet
- Consensus: P0.0 phase 7 (governance-scheduled activation), M2 timestamp bound, a2 short timeout, rotating the EVM proposer (Q12: N=11)
- Product: P2 fees, mark price, bands, funding, isolated margin; P3 order surface; P4 HL shim and state sync; P5 HLP vault, subaccounts, spot, bridge
- Perf: co-location, ingress dispersal, Block-STM, T5.1 (310 ms cadence), T5.2 (1.2 s p99 floor)

## Parked
- b1 / mode 3 / b0, qca-timers
- opt-level A/B, test-binary merge + hakari, sccache cap
- trading app rebuild (EIP-712 signer outdated)
