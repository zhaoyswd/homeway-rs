//! 仅本机管理入口使用的已认证注册快照；不增加线上协议。
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}, mpsc};
use std::time::Instant;

static NEXT_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct RegistryHandle(Arc<Inner>);
struct Inner {
    epoch: u64,
    snapshot: Mutex<Snapshot>,
    tx: mpsc::SyncSender<Release>,
    rx: Mutex<mpsc::Receiver<Release>>,
}
#[derive(Default)]
struct Snapshot { address: Option<SocketAddr>, routes: HashMap<[u8; 32], [u8; 8]>, associations: usize }
pub(super) struct Release { pub label: [u8; 8], pub client: SocketAddr, pub cutoff: Instant }

impl Default for RegistryHandle { fn default() -> Self { Self::new() } }
impl RegistryHandle {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel(32);
        Self(Arc::new(Inner { epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed), snapshot: Mutex::new(Snapshot::default()), tx, rx: Mutex::new(rx) }))
    }
    pub fn epoch(&self) -> u64 { self.0.epoch }
    pub fn address(&self) -> Option<SocketAddr> { self.0.snapshot.lock().unwrap_or_else(|e| e.into_inner()).address }
    pub fn lookup(&self, public_key: &[u8; 32]) -> Option<(SocketAddr, [u8; 8])> {
        let snapshot = self.0.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        Some((local_destination(snapshot.address?), *snapshot.routes.get(public_key)?))
    }
    pub fn contains(&self, public_key: &[u8; 32]) -> bool { self.lookup(public_key).is_some() }
    pub fn counts(&self) -> (usize, usize) { let s = self.0.snapshot.lock().unwrap_or_else(|e| e.into_inner()); (s.routes.len(), s.associations) }
    pub fn release_association(&self, epoch: u64, label: [u8; 8], client: SocketAddr, cutoff: Instant) -> bool {
        epoch == self.epoch() && self.address().is_some() && self.0.tx.try_send(Release { label, client, cutoff }).is_ok()
    }
    pub fn clear(&self) { *self.0.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot::default(); }
    pub(super) fn publish(&self, address: SocketAddr, routes: impl Iterator<Item=([u8;32], [u8;8])>, associations: usize) {
        *self.0.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot { address: Some(address), routes: routes.collect(), associations };
    }
    pub(super) fn take_releases(&self) -> Vec<Release> { self.0.rx.lock().unwrap_or_else(|e| e.into_inner()).try_iter().take(32).collect() }
}

pub fn local_destination(address: SocketAddr) -> SocketAddr {
    if address.ip().is_unspecified() { SocketAddr::new(match address.ip() { IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(), IpAddr::V6(_) => Ipv6Addr::LOCALHOST.into() }, address.port()) } else { address }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_identity_epoch_and_wildcard_are_exact() {
        let registry = RegistryHandle::new();
        assert!(registry.lookup(&[1;32]).is_none());
        registry.publish("0.0.0.0:1234".parse().unwrap(), std::iter::once(([1;32],[2;8])), 3);
        assert_eq!(registry.lookup(&[1;32]).unwrap().0, "127.0.0.1:1234".parse().unwrap());
        assert_eq!(local_destination("[::]:4321".parse().unwrap()), "[::1]:4321".parse().unwrap());
        assert!(!registry.release_association(registry.epoch()+1,[2;8],"127.0.0.1:1".parse().unwrap(),Instant::now()));
        registry.clear(); assert!(registry.lookup(&[1;32]).is_none());
    }

    #[test]
    fn release_queue_is_bounded_and_preserves_exact_identity_and_close_cutoff() {
        let registry = RegistryHandle::new();
        registry.publish("127.0.0.1:1234".parse().unwrap(), std::iter::once(([1; 32], [2; 8])), 1);
        let source = "127.0.0.1:4567".parse().unwrap();
        let cutoff = Instant::now();
        assert!(!registry.release_association(registry.epoch() + 1, [2; 8], source, cutoff));
        for _ in 0..32 {
            assert!(registry.release_association(registry.epoch(), [2; 8], source, cutoff));
        }
        assert!(!registry.release_association(registry.epoch(), [2; 8], source, cutoff));
        let releases = registry.take_releases();
        assert_eq!(releases.len(), 32);
        for release in releases {
            assert_eq!(release.label, [2; 8]);
            assert_eq!(release.client, source);
            assert_eq!(release.cutoff, cutoff);
        }
        assert!(registry.take_releases().is_empty());
        assert!(registry.release_association(registry.epoch(), [2; 8], source, cutoff));
        registry.clear();
        assert!(!registry.release_association(registry.epoch(), [2; 8], source, cutoff));
    }

}
