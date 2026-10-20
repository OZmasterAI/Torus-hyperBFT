//! s131: the collector's `qc_to_advance` step timers are wired into the live
//! algorithm loop. Three healthy validators commit; the installed observer
//! must receive samples whose steps sum to their total, with the view moved
//! by an AdvanceView (the collector's own copy comes back through the mock
//! network's broadcast self-delivery).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hotstuff_rs::qc_advance_timers::{set_observer, AdvancedBy, QcAdvanceSample};
use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

#[test]
fn qc_advance_samples_from_a_live_cluster() {
    let samples: Arc<Mutex<Vec<QcAdvanceSample>>> = Arc::default();
    let sink = samples.clone();
    assert!(set_observer(move |s| sink.lock().unwrap().push(s.clone())));

    let keypairs: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();
    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| Node::new(kp, net, init_as.clone(), vs_updates.clone()))
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    wait_until(
        Duration::from_secs(120),
        Duration::from_millis(200),
        "committed height >= 3 and >= 6 qc_advance samples",
        || {
            nodes.iter().all(|n| n.committed_height().unwrap_or(0) >= 3)
                && samples.lock().unwrap().len() >= 6
        },
        || format!("samples={}", samples.lock().unwrap().len()),
    );

    let samples = samples.lock().unwrap();
    for s in samples.iter() {
        let sum: Duration = s.steps.iter().sum();
        assert_eq!(sum, s.total, "steps must sum to total: {s:?}");
    }
    let by_advance_view = samples
        .iter()
        .filter(|s| s.advanced_by != AdvancedBy::Other)
        .count();
    assert!(
        by_advance_view * 2 > samples.len(),
        "most healthy views advance on an AdvanceView: {samples:?}"
    );
    assert!(
        samples.iter().any(|s| s.own_loopback.is_some()),
        "the collector's own AdvanceView comes back: {samples:?}"
    );
    assert!(
        samples.iter().any(|s| s.committed_blocks > 0),
        "some collector views commit and feed the app: {samples:?}"
    );
}
