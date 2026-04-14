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

## Operational Checklist

- [ ] Keystore file backed up securely
- [ ] Passphrase stored separately from keystore
- [ ] UDP 30333 open for P2P traffic
- [ ] Grafana dashboards imported and accessible
- [ ] Alerting rules configured (see `monitoring/alerts/`)
- [ ] Disk space monitored (consider `--retention-blocks` for non-archive nodes)
- [ ] Node time synchronized (NTP)
