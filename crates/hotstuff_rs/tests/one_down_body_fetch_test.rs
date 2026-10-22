//! 9k one-down finding (bench `bench-ozarchy-9k2`, stakes 2:2:1, the power-1 validator killed with
//! no restart): within 5-11 s of the kill a SURVIVOR could not fetch the OTHER survivor's block
//! body within the 4 s budget and fell back to block sync (5 of 7 standard K cells).
//!
//! # Mechanism
//!
//! `HotStuff::tick_pending_body_retries_at` asks the proposer (the header's sender) twice, then
//! rotates the remaining fast attempts over "the other committed validators", then alternates
//! proposer / others in the slow phase. In a 3-validator net with one validator dead the only
//! "other" is the dead node, so 10 of the 16 asks of the 4 s budget go to a peer that cannot
//! answer, and the live proposer — the only holder — is asked just 6 times (t = 0, 0.1, 0.2,
//! 1.4, 2.4, 3.4 s). With all validators up the same rotation reaches a second holder at 0.3 s,
//! which masks a stalled or lossy proposer link; with one down nothing does.
//!
//! # The test
//!
//! Three validators at 2:2:1 commit with everyone up; then the power-1 node is stopped (dropped:
//! everything sent to it is lost, as with a killed process). Once it has been silent for a few
//! seconds, the mock network loses the first [`LOST_ASKS`] body requests each survivor sends to
//! the live holder of a block proposed from then on (a serving stall of about half a second).
//! Every such fetch must still get its body well inside the 4 s budget, and no request may go to
//! the dead node.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use hotstuff_rs::{
    hotstuff::messages::HotStuffMessage,
    networking::messages::{Message, ProgressMessage},
    types::{
        crypto_primitives::{SigningKey, VerifyingKey},
        data_types::{CryptoHash, Power},
        update_sets::ValidatorSetUpdates,
    },
};

mod common;

use common::{
    network::mock_network_with_filter,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);

/// Asks to the live holder lost per fetch. The pre-fix schedule asks the live proposer exactly 6
/// times inside the 4 s budget, so losing 6 makes every fetch miss it; a schedule that does not
/// spend asks on the dead node reaches the 7th ask after ~0.6 s.
const LOST_ASKS: u32 = 6;

/// How long the dead node must have been silent before fetches are measured (survivors may still
/// count it as live just after it stops).
const SILENCE_BEFORE_MEASURING: Duration = Duration::from_secs(3);

/// A measured fetch must have its body within this (the fallback-to-sync budget is 4 s).
const BODY_WITHIN: Duration = Duration::from_millis(2500);

/// Measured fetches to observe before passing.
const FETCHES_TO_OBSERVE: usize = 3;

#[derive(Default)]
struct Fetch {
    first_ask: Option<Instant>,
    lost: u32,
    targets: Vec<VerifyingKey>,
    body_at: Option<Instant>,
}

struct Probe {
    dead: VerifyingKey,
    measuring_from: Instant,
    /// Blocks whose header was sent after `measuring_from`.
    measured_blocks: HashSet<CryptoHash>,
    /// `(requester, block)` -> fetch.
    fetches: HashMap<(VerifyingKey, CryptoHash), Fetch>,
    asks_to_dead: usize,
}

impl Probe {
    fn observe(&mut self, sender: &VerifyingKey, recipient: &VerifyingKey, message: Message) -> Option<Message> {
        let now = Instant::now();
        if now < self.measuring_from {
            return Some(message);
        }
        let Message::ProgressMessage(ProgressMessage::HotStuffMessage(hs)) = &message else {
            return Some(message);
        };
        match hs {
            HotStuffMessage::ProposalHeader(header) => {
                self.measured_blocks.insert(header.block_hash);
            }
            HotStuffMessage::BlockDataRequest(req) if self.measured_blocks.contains(&req.block_hash) => {
                if *recipient == self.dead {
                    self.asks_to_dead += 1;
                }
                let fetch = self.fetches.entry((*sender, req.block_hash)).or_default();
                fetch.first_ask.get_or_insert(now);
                fetch.targets.push(*recipient);
                if *recipient != self.dead && fetch.lost < LOST_ASKS {
                    fetch.lost += 1;
                    return None;
                }
            }
            HotStuffMessage::BlockDataResponse(resp) => {
                if let Some(fetch) = self.fetches.get_mut(&(*recipient, resp.block.hash)) {
                    fetch.body_at.get_or_insert(now);
                }
            }
            _ => {}
        }
        Some(message)
    }

    fn short(vk: &VerifyingKey, keys: &[VerifyingKey]) -> String {
        keys.iter().position(|k| k == vk).map_or("?".into(), |i| format!("n{i}"))
    }

    fn describe(&self, keys: &[VerifyingKey]) -> String {
        let now = Instant::now();
        let mut lines: Vec<String> = self
            .fetches
            .iter()
            .map(|((requester, hash), f)| {
                let first = f.first_ask.unwrap_or(now);
                format!(
                    "{} fetch {:02x?}: age {:?}, body after {:?}, asks {:?}",
                    Self::short(requester, keys),
                    &hash.bytes()[..4],
                    now - first,
                    f.body_at.map(|t| t - first),
                    f.targets.iter().map(|t| Self::short(t, keys)).collect::<Vec<_>>(),
                )
            })
            .collect();
        lines.sort();
        format!("asks to dead node: {}; fetches:\n    {}", self.asks_to_dead, lines.join("\n    "))
    }
}

#[test]
fn survivors_fetch_each_others_bodies_without_asking_the_dead_validator() {
    let keypairs: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let keys: Vec<VerifyingKey> = keypairs.iter().map(|kp| kp.verifying_key()).collect();
    let powers = [2_000_000u64, 2_000_000, 1_000_000];
    let mut vs_updates = ValidatorSetUpdates::new();
    for (vk, p) in keys.iter().zip(powers) {
        vs_updates.insert(*vk, Power::new(p));
    }
    let init_as = NumberApp::initial_app_state();
    let (stubs, filter) = mock_network_with_filter(keys.iter().copied());

    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_max_view_time(kp, net, init_as.clone(), vs_updates.clone(), MAX_VIEW_TIME)
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    // All up: every validator commits.
    wait_until(
        Duration::from_secs(60),
        POLL_INTERVAL,
        "all three validators to commit height >= 1",
        || nodes.iter().all(|n| n.committed_height().unwrap_or(0) >= 1),
        || format!("{:?}", nodes.iter().map(|n| n.committed_height()).collect::<Vec<_>>()),
    );

    // Kill the power-1 validator (no restart).
    drop(nodes.pop());
    let probe = Arc::new(Mutex::new(Probe {
        dead: keys[2],
        measuring_from: Instant::now() + SILENCE_BEFORE_MEASURING,
        measured_blocks: HashSet::new(),
        fetches: HashMap::new(),
        asks_to_dead: 0,
    }));
    let observer = probe.clone();
    filter.rewrite(Arc::new(move |sender, recipient, message| {
        observer.lock().unwrap_or_else(|e| e.into_inner()).observe(sender, recipient, message)
    }));

    let check = |p: &Probe| -> Result<bool, String> {
        let now = Instant::now();
        if p.asks_to_dead > 0 {
            return Err(format!("{} body request(s) went to the dead validator", p.asks_to_dead));
        }
        let mut done = 0;
        for f in p.fetches.values() {
            let first = f.first_ask.expect("tracked fetches have an ask");
            match f.body_at {
                Some(at) if at - first <= BODY_WITHIN => done += 1,
                Some(at) => return Err(format!("a body took {:?} (bound {BODY_WITHIN:?})", at - first)),
                None if now - first > BODY_WITHIN => {
                    return Err(format!("no body after {:?} (bound {BODY_WITHIN:?})", now - first))
                }
                None => {}
            }
        }
        Ok(done >= FETCHES_TO_OBSERVE)
    };

    let deadline = Instant::now() + Duration::from_secs(90);
    let failure = loop {
        let p = probe.lock().unwrap();
        match check(&p) {
            Ok(true) => break None,
            Ok(false) if Instant::now() < deadline => {}
            Ok(false) => {
                break Some(format!(
                    "fewer than {FETCHES_TO_OBSERVE} measured fetches completed in 90 s\n  {}",
                    p.describe(&keys)
                ))
            }
            Err(why) => break Some(format!("one-down body fetch failed: {why}\n  {}", p.describe(&keys))),
        }
        drop(p);
        std::thread::sleep(POLL_INTERVAL);
    };
    // Release the probe and stop the survivors before failing, so a failure reports cleanly.
    filter.heal();
    drop(nodes);
    if let Some(failure) = failure {
        panic!("{failure}");
    }
}
