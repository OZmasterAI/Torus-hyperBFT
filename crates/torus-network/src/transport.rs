//! Dial-layer address filtering — the last line of defense behind the
//! kademlia book hygiene in `swarm.rs`.
//!
//! Kademlia dials query candidates straight out of FIND_NODE responses:
//! those addresses never pass the identify-insertion filter and are dialed
//! before the `RoutingUpdated` eviction can remove them. Any connected peer
//! that still carries stale private records (a pre-fix build, an old devnet
//! address book) can therefore keep triggering dials to loopback/RFC1918
//! endpoints no matter how clean our own book is. Rejecting the dial itself
//! closes every path.

use std::{
    collections::HashSet,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use libp2p::{
    core::transport::{DialOpts, ListenerId, TransportError, TransportEvent},
    multiaddr::Protocol,
    Multiaddr,
};

use crate::config::is_global_addr;

/// Wraps a transport and refuses dials to non-global addresses
/// (loopback / RFC1918 / link-local / CGNAT / ULA).
///
/// Mirrors [`crate::config::NetworkConfig::allow_private_addrs`]: with the
/// flag set (devnet / single-host fabrics) `enforce` is false and every dial
/// passes through. Explicitly configured bootstrap peers are always dialable
/// regardless — operator intent wins.
pub struct GlobalOnlyTransport<T> {
    inner: T,
    enforce: bool,
    /// Operator-configured bootstrap addresses, stored without any trailing
    /// `/p2p/<peer>` component.
    exempt: Arc<HashSet<Multiaddr>>,
}

impl<T> GlobalOnlyTransport<T> {
    pub fn new(inner: T, enforce: bool, exempt: Arc<HashSet<Multiaddr>>) -> Self {
        Self {
            inner,
            enforce,
            exempt,
        }
    }
}

/// Strip a trailing `/p2p/<peer>` component so dialed addresses (which the
/// swarm suffixes with the target peer id) compare equal to configured ones.
pub fn strip_p2p(addr: &Multiaddr) -> Multiaddr {
    let mut a = addr.clone();
    if matches!(a.iter().last(), Some(Protocol::P2p(_))) {
        a.pop();
    }
    a
}

impl<T: libp2p::Transport + Unpin> libp2p::Transport for GlobalOnlyTransport<T> {
    type Output = T::Output;
    type Error = T::Error;
    type ListenerUpgrade = T::ListenerUpgrade;
    type Dial = T::Dial;

    fn listen_on(
        &mut self,
        id: ListenerId,
        addr: Multiaddr,
    ) -> Result<(), TransportError<Self::Error>> {
        self.inner.listen_on(id, addr)
    }

    fn remove_listener(&mut self, id: ListenerId) -> bool {
        self.inner.remove_listener(id)
    }

    fn dial(
        &mut self,
        addr: Multiaddr,
        opts: DialOpts,
    ) -> Result<Self::Dial, TransportError<Self::Error>> {
        if self.enforce && !is_global_addr(&addr) && !self.exempt.contains(&strip_p2p(&addr)) {
            tracing::debug!(%addr, "refusing dial to non-global address");
            return Err(TransportError::MultiaddrNotSupported(addr));
        }
        self.inner.dial(addr, opts)
    }

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use libp2p::core::transport::PortUse;
    use libp2p::core::Endpoint;
    use libp2p::{PeerId, Transport};

    use super::*;

    /// Inner transport that records dial attempts and never connects.
    #[derive(Clone, Default)]
    struct RecordingInner {
        dialed: Arc<Mutex<Vec<Multiaddr>>>,
    }

    impl Transport for RecordingInner {
        type Output = ();
        type Error = std::io::Error;
        type ListenerUpgrade = futures::future::Pending<Result<(), std::io::Error>>;
        type Dial = futures::future::Pending<Result<(), std::io::Error>>;

        fn listen_on(
            &mut self,
            _: ListenerId,
            _: Multiaddr,
        ) -> Result<(), TransportError<Self::Error>> {
            Ok(())
        }

        fn remove_listener(&mut self, _: ListenerId) -> bool {
            false
        }

        fn dial(
            &mut self,
            addr: Multiaddr,
            _: DialOpts,
        ) -> Result<Self::Dial, TransportError<Self::Error>> {
            self.dialed.lock().unwrap().push(addr);
            Ok(futures::future::pending())
        }

        fn poll(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<TransportEvent<Self::ListenerUpgrade, Self::Error>> {
            Poll::Pending
        }
    }

    fn opts() -> DialOpts {
        DialOpts {
            role: Endpoint::Dialer,
            port_use: PortUse::New,
        }
    }

    fn addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    fn filtered(
        enforce: bool,
        exempt: &[&str],
    ) -> (
        GlobalOnlyTransport<RecordingInner>,
        Arc<Mutex<Vec<Multiaddr>>>,
    ) {
        let inner = RecordingInner::default();
        let dialed = inner.dialed.clone();
        let exempt: Arc<HashSet<Multiaddr>> = Arc::new(exempt.iter().map(|s| addr(s)).collect());
        (GlobalOnlyTransport::new(inner, enforce, exempt), dialed)
    }

    #[test]
    fn rejects_non_global_dials() {
        let (mut t, dialed) = filtered(true, &[]);
        for s in [
            "/ip4/127.0.0.1/udp/30334/quic-v1",   // devnet loopback
            "/ip4/172.28.0.20/udp/30337/quic-v1", // devnet docker subnet
            "/ip4/192.168.1.5/tcp/30333",
            "/ip6/fd00::1/udp/30333/quic-v1",
        ] {
            let a = addr(s);
            match t.dial(a.clone(), opts()) {
                Err(TransportError::MultiaddrNotSupported(rejected)) => assert_eq!(rejected, a),
                other => panic!("{s}: expected MultiaddrNotSupported, got {other:?}"),
            }
        }
        assert!(
            dialed.lock().unwrap().is_empty(),
            "inner must never see filtered dials"
        );
    }

    #[test]
    fn rejects_non_global_with_p2p_suffix() {
        let (mut t, dialed) = filtered(true, &[]);
        let a = addr("/ip4/127.0.0.1/udp/30334/quic-v1").with(Protocol::P2p(PeerId::random()));
        assert!(t.dial(a, opts()).is_err());
        assert!(dialed.lock().unwrap().is_empty());
    }

    #[test]
    fn passes_global_and_dns_dials() {
        let (mut t, dialed) = filtered(true, &[]);
        for s in [
            "/ip4/84.32.108.220/udp/30333/quic-v1", // val1
            "/dns4/seed.example.org/udp/30333/quic-v1",
        ] {
            assert!(t.dial(addr(s), opts()).is_ok(), "{s} must pass");
        }
        assert_eq!(dialed.lock().unwrap().len(), 2);
    }

    #[test]
    fn disabled_passes_everything() {
        let (mut t, dialed) = filtered(false, &[]);
        assert!(t
            .dial(addr("/ip4/127.0.0.1/udp/30334/quic-v1"), opts())
            .is_ok());
        assert_eq!(dialed.lock().unwrap().len(), 1);
    }

    #[test]
    fn exempt_bootstrap_addr_passes_with_and_without_p2p() {
        let (mut t, dialed) = filtered(true, &["/ip4/192.168.1.5/udp/30333/quic-v1"]);
        let bare = addr("/ip4/192.168.1.5/udp/30333/quic-v1");
        let suffixed = bare.clone().with(Protocol::P2p(PeerId::random()));
        assert!(t.dial(bare, opts()).is_ok());
        assert!(t.dial(suffixed, opts()).is_ok());
        // A different private addr is still rejected.
        assert!(t
            .dial(addr("/ip4/192.168.1.6/udp/30333/quic-v1"), opts())
            .is_err());
        assert_eq!(dialed.lock().unwrap().len(), 2);
    }
}
