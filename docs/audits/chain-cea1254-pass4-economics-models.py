#!/usr/bin/env python3
"""Source-bound counterexample models; these do not execute Torus Rust code."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
REVISION = "cea1254e34625e6b09c58f794de8793b5c12713c"
assert subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip() == REVISION


def source(path):
    return (ROOT / path).read_text()


gov = source("crates/torus-economics/src/governance.rs")
genesis = source("crates/torus-genesis/src/lib.rs")
rewards = source("crates/torus-economics/src/rewards.rs")
app = source("crates/torus-consensus/src/app.rs")
native = source("crates/torus-bridge/src/native_executor.rs")

# E01: the executed change and the effective configuration have different keys.
assert 'GOVERNANCE_PARAMS_KEY: &[u8] = b"gov_params"' in gov
assert '.put_cf_raw(CF_FEE_CONFIG, param_key.as_bytes(), new_value.as_bytes())?' in gov
assert '.get_cf_raw(CF_FEE_CONFIG, GOVERNANCE_PARAMS_KEY)?' in gov
assert '"quorum_bps" =>' in gov and '!(1000..=6700).contains(&v)' in gov
store = {b"gov_params": {"quorum_bps": 3300}}
store[b"quorum_bps"] = b"5000"  # Successful execute_payload write.
effective_quorum = store[b"gov_params"]["quorum_bps"]
total_staked, yes_weight = 10_000, 4_000
assert yes_weight >= total_staked * effective_quorum // 10_000
assert yes_weight < total_staked * 5_000 // 10_000
print("E01: Executed quorum=5000 write leaves effective quorum=3300; 40% still passes")

# E02: malformed but deserializable genesis commission crosses unsigned zero.
assert 'pub commission_bps: u16' in genesis
assert 'commission_bps: validator.commission_bps' in genesis
assert 'let delegator_pool = val_emission - commission;' in rewards
assert 'staking.credit_rewards(del.delegator, share)?' in rewards
assert 'version = "1.17.2"' in source("Cargo.lock")
assert 'c141e807189ad38a07276942c6623032d3753c8859c146104ac2e4d68865945a' in source("Cargo.lock")
MOD = 1 << 256
WEI = 10**18
epoch_length = 100_000
blocks_per_year = 365 * 24 * 3600 // 2
stakes = [10_001 * WEI, 10_000 * WEI, 10_000 * WEI, 10_000 * WEI]
emission = sum(stakes) * 500 * epoch_length // (blocks_per_year * 10_000)
validator_emission = emission * stakes[0] // sum(stakes)
commission = validator_emission * 20_000 // 10_000
delegator_pool = (validator_emission - commission) % MOD
# A sole real delegator takes the full pool through the last-row branch.
pending_validator = commission
pending_delegator = delegator_pool
assert validator_emission > 0
assert pending_delegator == MOD - validator_emission
assert pending_validator + pending_delegator == MOD + validator_emission
# A U256-only conservation sum would mask the extra 2**256 of liabilities.
assert (pending_validator + pending_delegator) % MOD == validator_emission
delegator_liquid_after_claim = pending_delegator  # Zero pre-claim liquid balance.
assert delegator_liquid_after_claim > MOD // 2
print(f"E02: epoch validator emission={validator_emission}; sole delegator liability=2**256-emission")

# E03: governance work is absent from the native-phase scheduling gate.
gate = app.split('let run_native = if has_native', 1)[1].split('if run_native', 1)[0]
assert 'EpochManager::is_epoch_boundary(height, self.epoch_length)' in gate
assert 'NativeExecutor::oracle_due(&overlay)' in gate
assert 'governance' not in gate.lower()
assert 'NativeExecutor::process_governance(&mut ctx);' in app
assert 'ctx.governance.process_pending_proposals(ctx.block_height)' in native
voting_period = 7 * 24 * 3600 // 2
timelock = 24 * 3600 // 2
start = 1
end = start + voting_period
expected_finalization = end + 1
actual_finalization = ((end // epoch_length) + 1) * epoch_length
expected_execution = expected_finalization + timelock
actual_eligible = actual_finalization + timelock
actual_execution = ((actual_eligible + epoch_length - 1) // epoch_length) * epoch_length
assert (expected_finalization, actual_finalization) == (302_402, 400_000)
assert (expected_execution, actual_execution) == (345_602, 500_000)
print("E03: quiet-chain finalization302402->400000; execution345602->500000")
print("All three source-guarded arithmetic/control-flow models passed; no Rust runtime reproduction")
