//! Web 管理认证的同步公面与定长字节协议；不包含网络设备准入材料。
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::ServiceIntakeTx;

pub const VERSION: u8 = 1;
pub const HELLO_MAGIC: [u8; 2] = *b"W1";
pub const HELLO_LEN: usize = 19;
pub const CHALLENGE_LEN: usize = 19;
pub const PROOF_LEN: usize = 51;
pub const ACCEPT: [u8; 3] = *b"WA\x01";
pub const EXPORTER_LABEL: &[u8] = b"homeway-web-exporter-v1";
pub const AUTH_BUDGET: Duration = Duration::from_secs(5);
pub const MAX_SESSIONS: usize = 8;
pub const REVOKE_POLL: Duration = Duration::from_millis(100);
pub const LEG_RETRY: Duration = Duration::from_millis(20);

/// QUIC application close code；与 protocol.md 一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[repr(u32)]
pub enum WebClose {
    Authentication = 0x70,
    Protocol = 0x71,
    Deadline = 0x72,
    Capacity = 0x73,
    Revoked = 0x74,
    Relay = 0x75,
    Unavailable = 0x76,
}

/// Web 独立秘密；刻意不与联网 TokenSecret 互换。
pub struct WebSecret([u8; 32]);

impl WebSecret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl Clone for WebSecret {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}
impl fmt::Debug for WebSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebSecret(<redacted>)")
    }
}
impl Drop for WebSecret {
    fn drop(&mut self) {
        for b in &mut self.0 {
            // SAFETY: 字节属于 self；volatile 防止敏感缓冲擦除被优化。
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

#[derive(Clone, Debug)]
pub struct WebCredential {
    key_id: [u8; 16],
    secret: WebSecret,
}
impl WebCredential {
    pub fn new(key_id: [u8; 16], secret: WebSecret) -> Self {
        Self { key_id, secret }
    }
    pub fn key_id(&self) -> &[u8; 16] {
        &self.key_id
    }
    pub fn secret(&self) -> &WebSecret {
        &self.secret
    }
    pub fn proof(&self, nonce: &[u8; 16], exporter: &[u8; 32]) -> [u8; PROOF_LEN] {
        let mut out = [0; PROOF_LEN];
        out[..3].copy_from_slice(b"WP\x01");
        out[3..19].copy_from_slice(&self.key_id);
        out[19..].copy_from_slice(
            &mac(&self.secret, &self.key_id, nonce, exporter)
                .finalize()
                .into_bytes(),
        );
        out
    }
}

fn mac(secret: &WebSecret, id: &[u8; 16], nonce: &[u8; 16], exporter: &[u8; 32]) -> Hmac<Sha256> {
    let mut h =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    h.update(b"homeway-web-auth-v1");
    h.update(&[VERSION]);
    h.update(id);
    h.update(nonce);
    h.update(exporter);
    h
}

pub fn hello(key_id: &[u8; 16]) -> [u8; HELLO_LEN] {
    let mut out = [0; HELLO_LEN];
    out[..3].copy_from_slice(b"W1\x01");
    out[3..].copy_from_slice(key_id);
    out
}

fn equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in a.iter().zip(b) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 同一凭证代际的撤销活位；Drop 不撤销其他使用者。
#[derive(Clone, Debug)]
pub struct WebGrant(Arc<AtomicBool>);
impl WebGrant {
    pub fn is_valid(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct Installed {
    credential: WebCredential,
    grant: WebGrant,
}

/// 即使尚未生成凭证也恒装配该句柄，首次取 token 可热安装。
#[derive(Clone, Default)]
pub struct WebAuth(Arc<Mutex<Option<Installed>>>);
impl WebAuth {
    pub fn empty() -> Self {
        Self::default()
    }
    pub fn is_configured(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
    pub fn install(&self, credential: WebCredential) -> WebGrant {
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = current.as_ref() {
            if old.credential.key_id == credential.key_id
                && equal(
                    old.credential.secret.as_bytes(),
                    credential.secret.as_bytes(),
                )
            {
                return old.grant.clone();
            }
            old.grant.0.store(false, Ordering::Release);
        }
        let grant = WebGrant(Arc::new(AtomicBool::new(true)));
        *current = Some(Installed {
            credential,
            grant: grant.clone(),
        });
        grant
    }
    /// 已可见的凭据持久化失败且无法读取新值时，显式撤销现有授权。
    pub fn clear(&self) {
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(installed) = current.take() {
            installed.grant.0.store(false, Ordering::Release);
        }
    }
    pub fn authenticate(&self, key_id: &[u8; 16], secret: &[u8; 32]) -> Option<WebGrant> {
        let current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let installed = current.as_ref()?;
        (equal(&installed.credential.key_id, key_id)
            & equal(installed.credential.secret.as_bytes(), secret))
        .then(|| installed.grant.clone())
    }
    pub(crate) fn verify(
        &self,
        key_id: &[u8; 16],
        nonce: &[u8; 16],
        exporter: &[u8; 32],
        proof: &[u8; PROOF_LEN],
    ) -> Option<WebGrant> {
        let current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let installed = current.as_ref()?;
        if &proof[..3] != b"WP\x01"
            || !equal(&proof[3..19], key_id)
            || !equal(&installed.credential.key_id, key_id)
        {
            return None;
        }
        mac(&installed.credential.secret, key_id, nonce, exporter)
            .verify_slice(&proof[19..])
            .ok()?;
        Some(installed.grant.clone())
    }
}

#[derive(Clone)]
pub struct WebManagementConfig {
    pub auth: WebAuth,
    pub intake: ServiceIntakeTx,
    pub(crate) active: Arc<AtomicUsize>,
}
impl WebManagementConfig {
    pub fn new(auth: WebAuth, intake: ServiceIntakeTx) -> Self {
        Self {
            auth,
            intake,
            active: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn active_sessions(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

/// 固定连接所有权活位。跨线程关闭不依赖可满的消息队列。
#[derive(Clone, Debug)]
pub struct ManagementLease {
    conn_id: u64,
    alive: Arc<AtomicBool>,
}
impl ManagementLease {
    pub fn new(conn_id: u64) -> Self {
        Self {
            conn_id,
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
    pub fn conn_id(&self) -> u64 {
        self.conn_id
    }
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.alive.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientConfirmation {
    Confirmed,
    LegMissing,
    GenerationMismatch,
    CapacityFull,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field<'a>(text: &'a str, key: &str) -> &'a str {
        text.split_once(&format!("\"{key}\""))
            .unwrap()
            .1
            .trim_start()
            .strip_prefix(':')
            .unwrap()
            .trim_start()
            .strip_prefix('"')
            .unwrap()
            .split('"')
            .next()
            .unwrap()
    }
    fn bytes(text: &str) -> Vec<u8> {
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
    #[test]
    fn independently_generated_management_golden_is_exact() {
        let fixture = include_str!("../../../fixtures/webserve/management_auth.json");
        let key_id: [u8; 16] = bytes(field(fixture, "key_id_hex")).try_into().unwrap();
        let secret: [u8; 32] = bytes(field(fixture, "secret_hex")).try_into().unwrap();
        let nonce: [u8; 16] = bytes(field(fixture, "nonce_hex")).try_into().unwrap();
        let exporter: [u8; 32] = bytes(field(fixture, "exporter_hex")).try_into().unwrap();
        let credential = WebCredential::new(key_id, WebSecret::from_bytes(secret));
        assert_eq!(
            hello(&key_id).as_slice(),
            bytes(field(fixture, "hello_hex"))
        );
        assert_eq!(
            credential.proof(&nonce, &exporter).as_slice(),
            bytes(field(fixture, "proof_hex"))
        );
        assert_eq!(ACCEPT.as_slice(), bytes(field(fixture, "accept_hex")));
    }
    #[test]
    fn wire_and_proof_are_connection_bound() {
        let credential = WebCredential::new([1; 16], WebSecret::from_bytes([2; 32]));
        let auth = WebAuth::empty();
        auth.install(credential.clone());
        assert_eq!(&hello(&[1; 16])[..3], b"W1\x01");
        let proof = credential.proof(&[3; 16], &[4; 32]);
        assert!(auth.verify(&[1; 16], &[3; 16], &[4; 32], &proof).is_some());
        assert!(auth.verify(&[1; 16], &[3; 16], &[5; 32], &proof).is_none());
        assert!(auth.verify(&[1; 16], &[5; 16], &[4; 32], &proof).is_none());
        assert!(auth.verify(&[5; 16], &[3; 16], &[4; 32], &proof).is_none());
        let mut broken = proof;
        broken[2] = 2;
        assert!(auth.verify(&[1; 16], &[3; 16], &[4; 32], &broken).is_none());
    }
    #[test]
    fn clear_revokes_shared_grants_and_reinstall_never_revives_old_grant() {
        let auth = WebAuth::empty();
        let shared = auth.clone();
        let credential = WebCredential::new([1; 16], WebSecret::from_bytes([2; 32]));
        let old = auth.install(credential.clone());
        shared.clear();
        assert!(!old.is_valid());
        assert!(!auth.is_configured());
        assert!(auth.authenticate(&[1; 16], &[2; 32]).is_none());
        let fresh = auth.install(credential);
        assert!(fresh.is_valid());
        assert!(!old.is_valid());
        auth.clear();
        auth.clear();
        assert!(!fresh.is_valid());
        assert!(!shared.is_configured());
    }
    #[test]
    fn install_is_idempotent_but_rotation_revokes() {
        let auth = WebAuth::empty();
        assert!(!auth.is_configured());
        let credential = WebCredential::new([1; 16], WebSecret::from_bytes([2; 32]));
        let grant = auth.install(credential.clone());
        auth.install(credential);
        assert!(grant.is_valid());
        assert!(auth.authenticate(&[1; 16], &[2; 32]).is_some());
        assert!(auth.authenticate(&[1; 16], &[3; 32]).is_none());
        auth.install(WebCredential::new([3; 16], WebSecret::from_bytes([4; 32])));
        assert!(!grant.is_valid());
        assert!(auth.authenticate(&[1; 16], &[2; 32]).is_none());
        assert!(!format!("{:?}", WebSecret::from_bytes([42; 32])).contains("42"));
    }
}
