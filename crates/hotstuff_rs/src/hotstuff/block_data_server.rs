//! s72 option 3: serve [`BlockDataRequest`]s off the algorithm thread.
//!
//! The algorithm thread handles one progress message per loop pass and can be
//! busy for 100 ms to 1+ s (validate, the app commit feed, a blocking exec
//! dispatch), so a body request queued behind it can outlive the requester's
//! retry budget. The poller routes body requests here instead (on by default;
//! `TORUS_BODY_SERVE_THREAD=0` restores the algorithm-thread path): the block is read from a [`BlockTreeCamera`]
//! snapshot (as the block-sync server does) and sent straight back. A miss is
//! forwarded unchanged to the algorithm thread, whose handler also covers
//! bodies held only in memory (`pending_bodies`).

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ed25519_dalek::VerifyingKey;

use crate::block_tree::{accessors::public::BlockTreeCamera, pluggables::KVStore};
use crate::hotstuff::messages::{BlockDataRequest, BlockDataResponse, HotStuffMessage};
use crate::logging::{body_fetch_trace_enabled, BodyFetchTraceId, BodyFetchTraceStamp};
use crate::networking::{messages::ProgressMessage, network::Network, sending::SenderHandle};
use crate::types::data_types::ChainID;

/// How often an idle server checks for shutdown.
const IDLE_WAIT: Duration = Duration::from_millis(50);

/// What [`BlockDataServer::handle`] did with a request.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Served {
    /// The block was in the tree and was sent to the requester.
    Sent,
    /// Not in the tree: handed to the algorithm thread unchanged.
    Forwarded,
    /// Another chain's request: dropped, as the progress filter would.
    WrongChain,
}

pub(crate) struct BlockDataServer<N: Network + 'static, K: KVStore> {
    chain_id: ChainID,
    camera: BlockTreeCamera<K>,
    requests: Receiver<(VerifyingKey, BlockDataRequest)>,
    fallback: Sender<(VerifyingKey, ProgressMessage)>,
    sender: SenderHandle<N>,
    shutdown_signal: Receiver<()>,
}

impl<N: Network + 'static, K: KVStore> BlockDataServer<N, K> {
    pub(crate) fn new(
        chain_id: ChainID,
        camera: BlockTreeCamera<K>,
        requests: Receiver<(VerifyingKey, BlockDataRequest)>,
        fallback: Sender<(VerifyingKey, ProgressMessage)>,
        network: N,
        shutdown_signal: Receiver<()>,
    ) -> Self {
        Self {
            chain_id,
            camera,
            requests,
            fallback,
            sender: SenderHandle::new(network),
            shutdown_signal,
        }
    }

    pub(crate) fn start(mut self) -> JoinHandle<()> {
        thread::Builder::new()
            .name("hotstuff-bodyserve".into())
            .spawn(move || loop {
                match self.shutdown_signal.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => (),
                }
                match self.requests.recv_timeout(IDLE_WAIT) {
                    Ok((origin, req)) => {
                        self.handle(origin, req);
                    }
                    Err(RecvTimeoutError::Timeout) => (),
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            })
            .expect("spawn the block-data server thread")
    }

    /// Serve one request from a fresh snapshot, or forward it on a miss.
    pub(crate) fn handle(&mut self, origin: VerifyingKey, req: BlockDataRequest) -> Served {
        if req.chain_id != self.chain_id {
            return Served::WrongChain;
        }
        let entry = body_fetch_trace_enabled().then(BodyFetchTraceStamp::capture);
        // A read error is treated as a miss: the algorithm thread retries it.
        let block = match self.camera.snapshot().block(&req.block_hash) {
            Ok(Some(block)) => block,
            _ => {
                let _ = self.fallback.send((origin, HotStuffMessage::from(req).into()));
                return Served::Forwarded;
            }
        };
        if let Some(entry) = entry {
            let done = BodyFetchTraceStamp::capture();
            log::info!("body_fetch_diag serve: {} lookup_done_seq={} lookup_done_mono_us={} lookup_done_unix_us={} peer={} request_view={} local_view=- hash={} found=true height={:?} via=server",
                entry, done.seq, done.mono_us, done.unix_us,
                BodyFetchTraceId(origin.to_bytes()), req.view.int(),
                BodyFetchTraceId(req.block_hash.bytes()), Some(block.height.int()));
        }
        self.sender.send::<HotStuffMessage>(
            origin,
            BlockDataResponse {
                view: req.view,
                block,
            }
            .into(),
        );
        Served::Sent
    }
}

/// `TORUS_BODY_SERVE_THREAD`: on by default (s72-bst A/B: all cells pass,
/// throughput neutral, serve queue 4-6x lower); only `0` turns it off.
pub(crate) fn parse_body_serve_thread(raw: Option<String>) -> bool {
    !matches!(raw.as_deref().map(str::trim), Some("0"))
}

pub(crate) fn body_serve_thread_from_env() -> bool {
    parse_body_serve_thread(std::env::var("TORUS_BODY_SERVE_THREAD").ok())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use ed25519_dalek::VerifyingKey;

    use super::{BlockDataServer, Served};
    use crate::block_tree::accessors::internal::BlockTreeSingleton;
    use crate::block_tree::accessors::public::BlockTreeCamera;
    use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use crate::hotstuff::header_fast_path_regression_test::{signing_keys, validator_set};
    use crate::hotstuff::messages::{BlockDataRequest, HotStuffMessage};
    use crate::hotstuff::types::PhaseCertificate;
    use crate::networking::messages::{Message, ProgressMessage};
    use crate::networking::network::Network;
    use crate::types::block::Block;
    use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, ViewNumber};
    use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates};
    use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

    const CHAIN: ChainID = ChainID::new(0);

    /// In-memory KV whose clones share storage, so a camera sees what the
    /// block tree writes (production RocksDB behaves the same way).
    #[derive(Clone, Default)]
    struct SharedKV(Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>);

    struct SharedWb {
        sets: Vec<(Vec<u8>, Vec<u8>)>,
        deletes: Vec<Vec<u8>>,
    }

    struct SharedSnap(HashMap<Vec<u8>, Vec<u8>>);

    impl WriteBatch for SharedWb {
        fn new() -> Self {
            Self {
                sets: Vec::new(),
                deletes: Vec::new(),
            }
        }
        fn set(&mut self, key: &[u8], value: &[u8]) {
            self.sets.push((key.to_vec(), value.to_vec()));
        }
        fn delete(&mut self, key: &[u8]) {
            self.deletes.push(key.to_vec());
        }
    }

    impl KVGet for SharedKV {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.lock().unwrap().get(key).cloned()
        }
    }

    impl KVGet for SharedSnap {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.get(key).cloned()
        }
    }

    impl KVStore for SharedKV {
        type WriteBatch = SharedWb;
        type Snapshot<'a> = SharedSnap;
        fn write(&mut self, wb: SharedWb) {
            let mut map = self.0.lock().unwrap();
            for (k, v) in wb.sets {
                map.insert(k, v);
            }
            for k in wb.deletes {
                map.remove(&k);
            }
        }
        fn clear(&mut self) {
            self.0.lock().unwrap().clear();
        }
        fn snapshot(&self) -> SharedSnap {
            SharedSnap(self.0.lock().unwrap().clone())
        }
    }

    /// Records every point-to-point send.
    #[derive(Clone, Default)]
    struct SendLog(Arc<Mutex<Vec<(VerifyingKey, Message)>>>);

    impl Network for SendLog {
        fn init_validator_set(&mut self, _validator_set: ValidatorSet) {}
        fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {}
        fn broadcast(&mut self, _message: Message) {}
        fn send(&mut self, peer: VerifyingKey, message: Message) {
            self.0.lock().unwrap().push((peer, message));
        }
        fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
            None
        }
    }

    struct Fixture {
        block: Block,
        peer: VerifyingKey,
        net: SendLog,
        fallback: mpsc::Receiver<(VerifyingKey, ProgressMessage)>,
        server: BlockDataServer<SendLog, SharedKV>,
        requests: mpsc::Sender<(VerifyingKey, BlockDataRequest)>,
        shutdown: mpsc::Sender<()>,
    }

    /// A tree holding one block, and a server reading it through a camera.
    fn fixture() -> Fixture {
        let keys = signing_keys(&[1, 2, 3, 4]);
        let set = validator_set(&keys);
        let vss = ValidatorSetState::new(set.clone(), set, None, true);
        let kv = SharedKV::default();
        let mut tree = BlockTreeSingleton::new(kv.clone());
        tree.initialize(&AppStateUpdates::new(), &vss)
            .expect("block tree initialization");
        let block = Block::new(
            BlockHeight::new(0),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new([60u8; 32]),
            Data::new(vec![]),
        );
        tree.insert(&block, None, None).expect("insert block");
        let net = SendLog::default();
        let (to_fallback, fallback) = mpsc::channel();
        let (requests, request_rx) = mpsc::channel();
        let (shutdown, shutdown_rx) = mpsc::channel();
        let server = BlockDataServer::new(
            CHAIN,
            BlockTreeCamera::new(kv.clone()),
            request_rx,
            to_fallback,
            net.clone(),
            shutdown_rx,
        );
        Fixture {
            block,
            peer: keys[1].verifying_key(),
            net,
            fallback,
            server,
            requests,
            shutdown,
        }
    }

    fn request(chain_id: ChainID, block_hash: CryptoHash) -> BlockDataRequest {
        BlockDataRequest {
            chain_id,
            view: ViewNumber::new(7),
            block_hash,
        }
    }

    fn sent_response(net: &SendLog) -> Vec<(VerifyingKey, CryptoHash, ViewNumber)> {
        net.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(peer, msg)| match msg {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                    HotStuffMessage::BlockDataResponse(resp),
                )) => Some((*peer, resp.block.hash, resp.view)),
                _ => None,
            })
            .collect()
    }

    /// s72: on by default after the s72-bst A/B; only `0` turns it off.
    #[test]
    fn serve_thread_is_on_unless_explicitly_disabled() {
        use super::parse_body_serve_thread;
        assert!(parse_body_serve_thread(None));
        assert!(parse_body_serve_thread(Some("1".into())));
        assert!(parse_body_serve_thread(Some("".into())));
        assert!(parse_body_serve_thread(Some(" junk ".into())));
        assert!(!parse_body_serve_thread(Some("0".into())));
        assert!(!parse_body_serve_thread(Some(" 0 ".into())));
    }

    #[test]
    fn block_in_tree_is_sent_to_the_requester() {
        let mut f = fixture();
        let out = f.server.handle(f.peer, request(CHAIN, f.block.hash));
        assert_eq!(out, Served::Sent);
        assert_eq!(
            sent_response(&f.net),
            vec![(f.peer, f.block.hash, ViewNumber::new(7))],
            "the response must carry the requested block and the request's view"
        );
        assert!(f.fallback.try_recv().is_err(), "nothing forwarded");
    }

    #[test]
    fn miss_is_forwarded_to_the_algorithm_thread_unchanged() {
        let mut f = fixture();
        let missing = CryptoHash::new([61u8; 32]);
        let out = f.server.handle(f.peer, request(CHAIN, missing));
        assert_eq!(out, Served::Forwarded);
        assert!(sent_response(&f.net).is_empty(), "nothing sent on a miss");
        let (origin, msg) = f.fallback.try_recv().expect("forwarded request");
        assert_eq!(origin, f.peer);
        assert!(
            matches!(msg, ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(ref req))
                if *req == request(CHAIN, missing)),
            "the algo thread must get the original request"
        );
    }

    #[test]
    fn wrong_chain_is_dropped() {
        let mut f = fixture();
        let out = f.server.handle(f.peer, request(ChainID::new(9), f.block.hash));
        assert_eq!(out, Served::WrongChain);
        assert!(sent_response(&f.net).is_empty());
        assert!(f.fallback.try_recv().is_err());
    }

    /// When the poller goes away the server must exit and release its sender
    /// into the progress channel, so the algorithm still sees the disconnect.
    #[test]
    fn thread_exits_and_releases_fallback_when_requests_disconnect() {
        let f = fixture();
        let handle = f.server.start();
        drop(f.requests);
        handle.join().expect("server thread exits cleanly");
        assert!(matches!(
            f.fallback.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        drop(f.shutdown);
    }

    #[test]
    fn thread_serves_requests_and_exits_on_shutdown() {
        let f = fixture();
        let (net, peer, hash) = (f.net.clone(), f.peer, f.block.hash);
        let handle = f.server.start();
        f.requests
            .send((peer, request(CHAIN, hash)))
            .expect("server is listening");
        let deadline = Instant::now() + Duration::from_secs(5);
        while sent_response(&net).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sent_response(&net), vec![(peer, hash, ViewNumber::new(7))]);
        f.shutdown.send(()).expect("server alive");
        handle.join().expect("server thread exits cleanly");
    }
}
