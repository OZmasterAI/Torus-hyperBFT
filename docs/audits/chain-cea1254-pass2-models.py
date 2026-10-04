#!/usr/bin/env python3
"""Small source-guarded audit models, NOT execution of the Rust implementation.

Run from any directory with Python 3. These demonstrate consequences of the
reviewed predicates/arithmetic; they are not consensus or integration tests.
"""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
SHA = "cea1254e34625e6b09c58f794de8793b5c12713c"


def section(path, start, end):
    text = (ROOT / path).read_text()
    return text.split(start, 1)[1].split(end, 1)[0]


def source_guards():
    assert subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip() == SHA, "Re-audit models against the new revision"
    safe = section("crates/hotstuff_rs/src/block_tree/invariants.rs",
                   "pub(crate) fn safe_block", "/// Check whether `pc`")
    assert "safe_pc(&block.justify" in safe and "block.height" not in safe
    link = section("crates/torus-consensus/src/app.rs",
                   "fn check_parent_link(", "impl TorusApp")
    assert "header.height == parent.height + 1" in link
    feed = (ROOT / "crates/hotstuff_rs/src/committed_feed.rs").read_text()
    assert "for h in start..=highest.int()" in feed
    assert "None if anchored =>" in feed
    gov = "crates/torus-economics/src/governance.rs"
    weight = section(gov, "fn compute_vote_weight_at(", "fn snapshot_voter_weights(")
    assert "self.compute_vote_weight(voter, params)" in weight
    snapshot = section(gov, "fn snapshot_voter_weights(", "fn total_vote_weight(")
    assert "if !weight.is_zero()" in snapshot
    staking = "crates/torus-economics/src/staking.rs"
    register = section(staking, "pub fn register_validator(", "pub fn delegate(")
    assert "get_validator(&sender)" in register and "all_validators" not in register
    rotate = section(staking, "pub fn submit_key_rotation(", "pub fn get_pending_rotation(")
    assert "v.pubkey == new_pubkey" in rotate and "all_pending_rotations" not in rotate
    updates = (ROOT / "crates/hotstuff_rs/src/types/update_sets.rs").read_text()
    assert "self.inserts.insert(key, value)" in updates
    epoch = (ROOT / "crates/torus-economics/src/epoch.rs").read_text()
    assert "current_set_size / 3" in epoch
    assert "let half = max_changes / 2" in epoch


def height_gap():
    # A child has a valid inner link and correct parent hash but skips outer 8.
    parent = {"hash": "P", "outer": 7, "inner": 8}
    child = {"parent": "P", "outer": 9, "inner": 9}
    assert child["parent"] == parent["hash"]
    assert child["inner"] == parent["inner"] + 1
    assert child["outer"] != parent["outer"] + 1
    index = {7: "P", 9: "C"}
    fed, highest = 7, 9
    delivered = []
    for h in range(fed + 1, highest + 1):
        if h not in index:
            break
        delivered.append(index[h])
    assert delivered == []
    # Control: the same child indexed consecutively can be fed.
    assert {7: "P", 8: "C"}[fed + 1] == "C"
    print("F08: inner link valid; outer 8 absent; committed child 9 not delivered")


def governance_snapshot():
    snapshot = {"alice": 100}
    live = {"alice": 50, "bob": 200}
    def current_weight(voter):
        return snapshot[voter] if voter in snapshot else live.get(voter, 0)
    assert current_weight("alice") == 100  # Existing voters are frozen.
    assert current_weight("bob") == 200   # New voters bypass that freeze.
    assert snapshot.get("bob", 0) == 0
    print("F09: Bob's snapshot weight 0 becomes current voting weight 200")


def duplicate_keys():
    # Six installed validators plus a governance-approved newcomer G copying
    # A's key. F and G are controlled by the same adversary. Plan sorted by
    # power descending/address ascending; later inserts replace earlier keys.
    members = [("A", "keyA", 60), ("B", "keyB", 10),
               ("C", "keyC", 10), ("D", "keyD", 10),
               ("E", "keyE", 10), ("F", "keyF", 30),
               ("G", "keyA", 10)]
    installed = {}
    for address, key, power in sorted(members, key=lambda m: (-m[2], m[0])):
        installed[key] = power
    total_stake = sum(m[2] for m in members)
    assert total_stake == 140 and 3 * 40 < total_stake
    assert len(installed) == 6 and installed["keyA"] == 10
    actual_total = sum(installed.values())
    assert actual_total == 80 and 3 * installed["keyF"] > actual_total
    quorum = actual_total * 2 // 3 + 1
    assert actual_total - installed["keyF"] < quorum
    # Rotation checks only installed keys: two pending requests for unused X
    # both pass, because the first request does not modify installed keys.
    pending = {}
    for address in ("B", "C"):
        assert "unusedX" not in installed
        pending[address] = "unusedX"
    assert len(set(pending.values())) == 1
    print("F10: stake 140 -> consensus power 80; malicious share 40/140 -> 30/80")


def capped_membership(old, proposed):
    cap = len(old) // 3
    departures = sorted(old - proposed)
    arrivals = sorted(proposed - old)
    if len(departures) + len(arrivals) <= cap:
        return proposed
    swaps = min(cap // 2, len(departures), len(arrivals))
    return (proposed - set(arrivals[swaps:])) | set(departures[swaps:])


def rotation_cap():
    for n in (4, 5):
        old = set(range(n))
        proposed = old - {n - 1} | {n}
        for _ in range(10):
            actual = capped_membership(old, proposed)
            assert actual == old
            old = actual
    old = set(range(6))
    proposed = old - {5} | {6}
    assert capped_membership(old, proposed) == proposed  # Control: cap is 2.
    print("F11: full sets of 4 and 5 reject the same replacement for 10 epochs")


if __name__ == "__main__":
    source_guards()
    height_gap()
    governance_snapshot()
    duplicate_keys()
    rotation_cap()
