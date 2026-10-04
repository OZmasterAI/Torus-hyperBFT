#!/usr/bin/env python3
"""Source-guarded control-flow models, NOT Rust/RocksDB/revm reproductions.

Audited revision: cea1254e34625e6b09c58f794de8793b5c12713c.
The account model compares root inputs, not an Ethereum MPT implementation.
The restore model exercises real filesystem rename/mkdir only in a temporary dir.
"""
from pathlib import Path
from tempfile import TemporaryDirectory
import subprocess

ROOT = Path(__file__).resolve().parents[2]
AUDITED_REVISION = "cea1254e34625e6b09c58f794de8793b5c12713c"
actual_revision = subprocess.check_output(
    ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
).strip()
assert actual_revision == AUDITED_REVISION, (actual_revision, AUDITED_REVISION)


def require(path, *patterns):
    source = (ROOT / path).read_text()
    for pattern in patterns:
        assert pattern in source, (path, pattern)
    return source


app = require("crates/torus-consensus/src/app.rs",
              "overlay.flush_after_batch_with_native_trie_stats(",
              "resync_evm_accounts(&self.state_db, &native_evm_addrs)",
              "let _ = NativeExecutor::drain_core_writer(&mut ctx);",
              "ExecSource::Durable(height)")
require("crates/torus-state/src/incremental.rs",
        "if is_trie_built(db)? {", "let built = iter.valid();",
        "pub fn resync_evm_accounts", "if unique.is_empty() {")
require("crates/torus-bridge/src/state_root.rs",
        'Err(_) => true,',
        'if cfg!(debug_assertions) || std::env::var("TORUS_INCREMENTAL_ORACLE").is_ok()')
require("crates/torus-state/src/pruner.rs",
        "let cutoff = current_height.saturating_sub(self.config.retention_blocks);",
        "db.delete_range_cf(&cf, from_key, to_key)?;")
require("crates/torus-node/src/main.rs",
        "let latest_for_pruner = latest_height_handle.clone();",
        "match pruner.maybe_prune(current)",
        "std::fs::create_dir_all(&cli.data_dir)?;")
require("crates/hotstuff_rs/src/committed_feed.rs",
        "Some(fed) if fed >= highest => return Ok(0)")
require("crates/torus-core/src/precompiles.rs",
        "let entries = state.iterate_cf(CF_CORE_WRITER_QUEUE, Some(&prefix))?;",
        "state.delete_cf_raw(CF_CORE_WRITER_QUEUE, key)?;")
require("crates/torus-state/src/snapshot.rs",
        "std::fs::rename(data_dir, &old_dir)?;",
        "std::fs::rename(&temp_dir, data_dir)?;")

# A01: native withdrawal to an unrelated account, atomic plain-state+marker
# commit, then interruption before the separate derived-state resync.
plain = {"withdrawer": 0, "recipient": 7, "unrelated": 3}
mirrored = dict(plain)
plain["recipient"] += 5
applied = 10
# A fresh context drops its transient dirty list. A nonempty mirror passes boot
# existence check; replay does not revisit the already-applied height.
assert mirrored and applied == 10
assert plain["recipient"] == 12 and mirrored["recipient"] == 7
# A following unrelated bundle/resync does not repair recipient.
plain["unrelated"] = mirrored["unrelated"] = 4
assert plain != mirrored
print("A01: applied=10; recipient plain=12, mirror=7; unrelated update leaves drift")

# A02: healthy local lag, nonarchive retention=1, and a regular prune tick.
committed = fed = 1000
applied = 997
retention = 1
bodies = {h: f"nonempty-body-{h}" for h in range(990, 1001)}
manifests = {}  # removed atomically when these bodies were dispatched
tree = {h: f"compact-datum-{h}" for h in range(1, 1001)}
cutoff = committed - retention
bodies = {h: b for h, b in bodies.items() if h >= cutoff}
hole = applied + 1
assert hole not in bodies and hole not in manifests and hole in tree
assert fed >= committed  # boot feed returns 0, despite retained tree data
assert all(h in tree for h in range(1, committed + 1))  # no sync index gap
print("A02: retention=1 deletes replay body998; fed=1000 suppresses local refeed")

# H01 / F04 extension: an exact-height drain read error is ignored by app.
queue = {(11, 0): "already-burned lockbox deposit"}
applied = 10
fatal_error = None
drain_result = "read error"  # no pending deletions; Result ignored by caller
assert fatal_error is None
applied = 11  # ordinary overlay marker flush succeeds
next_prefix = applied + 1
assert not any(h == next_prefix for h, _ in queue)
assert (11, 0) in queue  # repeated future drains never retry height11
print("H01: ignored drain error advances marker11 and strands deposit under prefix11")

# H02: actual two-rename interruption seam. Simulate next ordinary startup's
# mkdir without opening a production DB or deleting any real user data.
with TemporaryDirectory(prefix="torus-pass4-restore-model-") as tmp:
    active = Path(tmp) / "data"
    old = active.with_suffix(".old")
    restoring = active.with_suffix(".restoring")
    active.mkdir()
    (active / "old-db-sentinel").write_text("valid old DB")
    restoring.mkdir()
    (restoring / "snapshot-sentinel").write_text("valid snapshot copy")
    active.rename(old)  # interrupt before restoring.rename(active)
    assert not active.exists() and old.exists() and restoring.exists()
    active.mkdir(parents=True, exist_ok=True)  # main.rs ordinary startup
    assert list(active.iterdir()) == []
    assert (old / "old-db-sentinel").exists()
print("H02: two-rename interruption leaves active absent; ordinary mkdir creates empty active")

print("All source guards and models passed; no production Rust or RocksDB behavior executed.")
