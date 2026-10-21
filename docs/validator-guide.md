# Validator Guide

## Key Management

### Generate a Keystore

```bash
./torus-node keygen --output validator.keystore
```

This generates an Ed25519 keypair and writes an encrypted keystore file.
You will be prompted for a passphrase. The public key is printed to stderr.

### Start with Keystore

```bash
./torus-node --keystore validator.keystore --genesis genesis.json
```

For automated deployments, use `--passphrase-file`:

```bash
echo "my-passphrase" > /etc/torus/passphrase
chmod 600 /etc/torus/passphrase
./torus-node --keystore validator.keystore --passphrase-file /etc/torus/passphrase
```

### Security Notes

- Never use `--validator-key` (raw hex) in production. It is deprecated and
  exposes the key in process listings and shell history.
- Store keystore files with restrictive permissions (`chmod 600`).
- Back up keystore files securely. Loss of the keystore means loss of the
  validator identity.

## Registration

Validator registration requires a governance proposal flow:

1. **Fund your validator address** with at least the minimum self-delegation
   (10,000 TRS = 10^22 wei).

2. **Register** via the `torus_registerValidator` RPC call, providing:
   - Your Ed25519 public key (32 bytes)
   - Commission rate in basis points (0-5000, i.e., 0%-50%)
   - Self-delegation amount (must be >= 10,000 TRS)

3. **Status progression**: Registered -> Candidate -> Active
   - Registration places you in Candidate status
   - At the next epoch boundary, the top validators by stake become Active
   - Maximum 21 active validators per epoch

## Self-Delegation

- **Minimum**: 10,000 TRS (`MIN_SELF_DELEGATION`)
- Registration fails if self-delegation is below the minimum
- If slashing reduces self-stake below the minimum, the validator is
  automatically jailed

## Commission

- **Maximum rate**: 50% (5000 bps)
- **Maximum change per update**: 1% (100 bps)
- **Cooldown between changes**: 28,800 blocks (`COMMISSION_COOLDOWN_BLOCKS`)
- Commission is taken from block rewards before distributing to delegators

### Update Commission

Use `torus_updateCommission` RPC call with the new rate in basis points.
The call will fail if:
- New rate exceeds 50%
- Change exceeds 1% from current rate
- Cooldown has not elapsed since last change

## Monitoring

### Grafana Dashboards

Import the dashboards from `monitoring/dashboards/`:

- `consensus.json`: Block height, consensus rounds, view progress
- `execution.json`: EVM transactions, gas usage, state root computation
- `network.json`: Peer count, network traffic

See [monitoring-setup.md](monitoring-setup.md) for setup instructions.

### Key Metrics to Watch

| Metric | Healthy | Concerning |
|--------|---------|------------|
| `torus_block_height` | Increasing steadily | Stalled |
| `torus_consensus_view` | Advancing | Stuck |
| `torus_peers_connected` | >= 3 | < 3 |
| `torus_block_build_seconds` | < 1s p99 | > 2s p99 |
| `torus_state_hash_unverified` | 0 | 1: no valid running-hash chain, the node submits no state-hash attestations (see [node-operator-guide.md](node-operator-guide.md#running-state-hash-a-hash-unverified-node)) |

## Jailing

### Triggers

A validator can be jailed for:

1. **Downtime / community vote**: Other validators submit jail votes. When
   vote weight exceeds threshold, the validator is slashed 0.1%
   (`DOWNTIME_SLASH_BPS = 10`) and jailed.

2. **Double signing**: Equivocation detected by consensus. Slashed 5%
   (`DOUBLE_SIGN_SLASH_BPS = 500`) and permanently tombstoned.

3. **Auto-jail**: If self-stake drops below 10,000 TRS after slashing.

### Jail Duration

- Standard jail: 28,800 blocks (`JAIL_DURATION_BLOCKS`)
- Tombstoned: permanent (cannot unjail)

### Unjailing

To unjail after the cooldown expires:

1. Ensure `self_stake >= 10,000 TRS` (top up if needed)
2. Wait for `jailed_until` block to pass
3. Call `torus_unjail` RPC method
4. Status changes to Candidate (must wait for next epoch to become Active)

## Key Rotation

Validators can rotate their Ed25519 consensus key:

1. Generate a new keystore with `torus-node keygen`
2. Submit the new public key via `torus_rotateKey` RPC
3. The rotation takes effect at the next epoch boundary
4. **Cooldown**: 1 epoch between rotations (`KEY_ROTATION_COOLDOWN_EPOCHS`)
5. **Safety cap**: At most floor(n/3) validators can rotate per epoch to
   maintain BFT quorum overlap

## Delisted Markets and the Price Feeder

When governance delists a market, the price feeder keeps publishing it through the grace period
and the settlement. The settlement price is the mark, or the last aggregated price, at the end of
the grace period, unless governance set a close price. Once the settlement is complete (no
position, no ADL obligation and no settlement row left), every validator ignores that market's
entries in `SubmitOraclePrices`. The rest of the submission is still stored, so nothing breaks.
The feeder sees this in `torus_getMarkets` (`settlement: "settled"`) and stops publishing the
market on its own (startup warning; omitted with reason `delist settled`). You can then remove it
from the feeder's `[[markets]]` config. Details:
[Configuration Reference](configuration-reference.md#market-delisting-governance).

## State Hash Attestations (`--state-hash-attest-key`)

When the genesis enables the running state hash (`consensus.state_hash_activation_height`), every
node hashes its state and keeps a checkpoint every 100 blocks. Validators attest each durable
checkpoint on chain with an `AttestStateHash` native action; a hash backed by > 2/3 of the
stake is the quorum hash, and every node compares it with its own checkpoint.

`--state-hash-attest-key <file>` gives the node the key that signs these attestations:

- **It is the validator's account key.** That is the secp256k1 key of the EVM address the
  validator is registered under (the address of `torus_registerValidator`, the one
  `torus_getValidators` lists). It is not the Ed25519 consensus key of `--keystore`.
- **The address must be the registered validator address, and that validator must be Active.**
  The node checks this before every submission. With any other key, or while the validator is
  not Active, the node submits nothing. It logs nothing either; the chain would reject the
  attestations. At startup the node logs `running state hash: automatic AttestStateHash enabled`
  with `addr=`. Check that this address is your validator address.
- **File format:** 64 hex characters, optional `0x`, mode 0600 or stricter. The node refuses a
  file that group or other can read. It is a file, not a value, so the key never shows in `ps`.
- **It is separate from the oracle signer.** The price feeder signs `SubmitOraclePrices` with
  its own hot oracle signer, registered with `torus-wallet set-oracle-signer`
  (`tools/price-feeder/README.md`). Never use the validator account key for oracle submissions.
  The chain's native replay protection is a set of (sender, nonce) pairs, not a counter
  (nonces are milliseconds, valid for ±60 s). If two actions from the same account carry the
  same millisecond nonce, only one gets in. An attestation that loses this race is lost until
  the node resubmits it (see below).

**Without the flag** the node never attests. It still hashes, still checks every vote and the
quorum hash against its own checkpoints (metrics `torus_state_hash_mismatch_total`,
`torus_state_hash_no_quorum_total`), and can still fail-stop with
`TORUS_STATE_HASH_FAILSTOP=1`. But its stake does not count toward any quorum. On a small
validator set (3 validators need all 3) a missing attester means no quorum hash at all.

**Resubmit after 65 s.** The node signs each attestation with nonce = now (ms; strictly
increasing per node) and submits it to its own mempool. If the vote is not on chain yet, the
node signs it again with a fresh nonce, but only once the previous signature can no longer
land: 60 s nonce window + 5 s clock-skew margin = 65 s (`DEFAULT_RESUBMIT_AFTER_MS`). An
attestation that is dropped or not admitted is therefore missing for about 65 s, not lost.
Log lines: `state hash attestation submitted` (`resubmission=true` on a retry), `state hash
attestation not admitted (retried after the resubmit window)`. Metric:
`torus_state_hash_attestations_submitted_total`.

**Hash-unverified.** A node becomes hash-unverified when it cannot extend a valid running-hash
chain: its database has no stored hash for the previous height. This happens when it was
upgraded or started above the activation height without a chain, or when a height was skipped
after a failed flush. The state is sticky. The node stops hashing and checkpointing, never
attests (even with the flag), and makes no fail-stop decisions. It still records and reports
mismatches. Metric `torus_state_hash_unverified` = 1. A hash-unverified node keeps following
the chain. To attest again it needs a data dir with a valid running-hash chain (resync).

## Operational Checklist

- [ ] Keystore file backed up securely
- [ ] Passphrase stored separately from keystore
- [ ] UDP 30333 open for P2P traffic
- [ ] Grafana dashboards imported and accessible
- [ ] Alerting rules configured (see `monitoring/alerts/`)
- [ ] Disk space monitored (consider `--retention-blocks` for non-archive nodes)
- [ ] Node time synchronized (NTP)
- [ ] Running state hash on: `--state-hash-attest-key` points at the validator ACCOUNT key
      (0600), the startup `addr=` equals the registered validator address, and the price
      feeder uses its own oracle signer, not this key
- [ ] Stop grace period longer than `TORUS_SHUTDOWN_TIMEOUT_SECS` (default
      60 s), e.g. 75 s: Docker `stop -t 75` / Compose `stop_grace_period`,
      Kubernetes `terminationGracePeriodSeconds`, systemd `TimeoutStopSec`.
      The defaults of Docker (10 s) and Kubernetes (30 s) kill the node before
      its clean shutdown ends. See "Stopping the Node" in the
      [Node Operator Guide](node-operator-guide.md#stopping-the-node).
