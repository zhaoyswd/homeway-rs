use std::io::{Read, Write};
use std::path::Path;

use base64::Engine;
use homeway_quic::{RpkPublicKey, WebCredential, WebSecret};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::Error;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    pub public_key: [u8; 32],
    pub pin: RpkPublicKey,
}

impl Identity {
    pub fn load(state: &Path) -> Result<Self, Error> {
        let state = homeway_core::server::state::State::open(&state.join("serve"))?;
        let key = state.private_key().map_err(Error::Identity)?;
        let public_key = x25519_dalek::PublicKey::from(&key).to_bytes();
        let pin = homeway_quic::rpk_public_key(&homeway_core::server::quic_rpk_seed(&key))
            .map_err(Error::Rpk)?;
        Ok(Self { public_key, pin })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TokenError {
    #[error("不支持的Web凭证版本")]
    UnsupportedVersion,
    #[error("Web凭证格式无效")]
    Malformed,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PersistError {
    #[error("Web凭证或配置写入失败，尚未替换原文件：{0}")]
    BeforeRename(#[source] std::io::Error),
    #[error("新文件已可见，但目录同步失败，掉电持久性未确认：{0}")]
    DurabilityUncertain(#[source] std::io::Error),
}
impl PersistError {
    pub fn commit_visible(&self) -> bool {
        matches!(self, Self::DurabilityUncertain(_))
    }
}

pub(super) struct Token {
    pub identity: Identity,
    pub credential: WebCredential,
}

impl Token {
    pub fn decode(text: &str) -> Result<Self, TokenError> {
        let text = text.trim();
        let body = text
            .strip_prefix("hww1")
            .ok_or(TokenError::UnsupportedVersion)?;
        if text.len() != 159 {
            return Err(TokenError::Malformed);
        }
        let raw = Zeroizing::new(B64.decode(body).map_err(|_| TokenError::Malformed)?);
        if raw.len() != 116 {
            return Err(TokenError::Malformed);
        }
        let mut sum = Sha256::new();
        sum.update(b"hww1");
        sum.update(&raw[..112]);
        if sum.finalize()[..4] != raw[112..] {
            return Err(TokenError::Malformed);
        }
        let identity = Identity {
            public_key: raw[..32].try_into().expect("长度已验"),
            pin: RpkPublicKey::from_bytes(raw[32..64].try_into().expect("长度已验")),
        };
        let credential = WebCredential::new(
            raw[64..80].try_into().expect("长度已验"),
            WebSecret::from_bytes(raw[80..112].try_into().expect("长度已验")),
        );
        Ok(Self {
            identity,
            credential,
        })
    }

    pub fn encode(&self) -> Zeroizing<String> {
        let mut raw = Zeroizing::new(Vec::with_capacity(116));
        raw.extend_from_slice(&self.identity.public_key);
        raw.extend_from_slice(self.identity.pin.as_bytes());
        raw.extend_from_slice(self.credential.key_id());
        raw.extend_from_slice(self.credential.secret().as_bytes());
        let mut sum = Sha256::new();
        sum.update(b"hww1");
        sum.update(&*raw);
        raw.extend_from_slice(&sum.finalize()[..4]);
        let mut text = Zeroizing::new(String::with_capacity(159));
        text.push_str("hww1");
        B64.encode_string(&*raw, &mut text);
        text
    }
}

pub(super) fn read(state: &Path) -> Result<Option<WebCredential>, Error> {
    use std::os::unix::fs::PermissionsExt;
    let file = match std::fs::File::open(state.join("web/auth.key")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // 校验实际打开文件的权限，再经同一句柄限长读取，避免路径检查/读取分离。
    if file.metadata()?.permissions().mode() & 0o7777 != 0o600 {
        return Err(Error::Invalid("web/auth.key权限必须为0600，请修正后重试"));
    }
    let mut raw = Zeroizing::new(Vec::with_capacity(54));
    file.take(54).read_to_end(&mut raw)?;
    if raw.len() != 53 || &raw[..5] != b"HWWK\x01" {
        return Err(Error::Invalid("web/auth.key损坏，请从备份恢复"));
    }
    Ok(Some(WebCredential::new(
        raw[5..21].try_into().expect("长度已验"),
        WebSecret::from_bytes(raw[21..].try_into().expect("长度已验")),
    )))
}

pub(super) fn obtain(state: &Path, rotate: bool) -> Result<WebCredential, Error> {
    let current = read(state)?;
    if !rotate {
        if let Some(c) = current {
            return Ok(c);
        }
    }
    let mut raw = Zeroizing::new([0u8; 53]);
    raw[..5].copy_from_slice(b"HWWK\x01");
    getrandom::getrandom(&mut raw[5..]).map_err(|_| Error::Random)?;
    atomic_write(&state.join("web/auth.key"), &*raw)?;
    Ok(WebCredential::new(
        raw[5..21].try_into().expect("长度已验"),
        WebSecret::from_bytes(raw[21..].try_into().expect("长度已验")),
    ))
}

pub(super) fn atomic_write(path: &Path, data: &[u8]) -> Result<(), Error> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let parent = path.parent().ok_or(Error::Invalid("配置路径无父目录"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    let mut random = [0u8; 8];
    getrandom::getrandom(&mut random).map_err(|_| Error::Random)?;
    let temp = parent.join(format!(
        ".web-{}-{:016x}.tmp",
        std::process::id(),
        u64::from_ne_bytes(random)
    ));
    // 在rename前打开实际父目录；若此步失败，旧文件仍未触碰。
    let directory = std::fs::File::open(parent).map_err(PersistError::BeforeRename)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        // OpenOptions的mode仍受umask影响；明确保证随后严格读校验所需的0600。
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(data)?;
        file.sync_all()?;
        #[cfg(test)]
        fail_at(FailurePoint::BeforeRename)?;
        std::fs::rename(&temp, path)?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(PersistError::BeforeRename(error).into());
    }
    // rename已提交到可见命名空间；此后的错误不能表示旧文件仍在，亦不尝试回滚新凭据。
    sync_directory(&directory).map_err(PersistError::DurabilityUncertain)?;
    Ok(())
}

fn sync_directory(directory: &std::fs::File) -> std::io::Result<()> {
    #[cfg(test)]
    fail_at(FailurePoint::DirectorySync)?;
    directory.sync_all()
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FailurePoint {
    BeforeRename,
    DirectorySync,
}
#[cfg(test)]
thread_local! {
    static WRITE_FAILURE: std::cell::Cell<Option<FailurePoint>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn fail_at(point: FailurePoint) -> std::io::Result<()> {
    if WRITE_FAILURE.with(|failure| failure.get() == Some(point)) {
        Err(std::io::Error::other(
            "injected private credential write failure",
        ))
    } else {
        Ok(())
    }
}
#[cfg(test)]
pub(super) fn with_write_failure<T>(point: FailurePoint, action: impl FnOnce() -> T) -> T {
    struct Restore(Option<FailurePoint>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WRITE_FAILURE.with(|failure| failure.set(self.0));
        }
    }
    let _restore = Restore(WRITE_FAILURE.with(|failure| failure.replace(Some(point))));
    action()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temporary() -> std::path::PathBuf {
        let mut id = [0; 8];
        getrandom::getrandom(&mut id).unwrap();
        std::env::temp_dir().join(format!("homeway-web-key-{:x}", u64::from_ne_bytes(id)))
    }
    #[test]
    fn independent_token_vectors_and_redacted_errors() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../fixtures/webserve/token_hww1.json"
        ))
        .unwrap();
        for case in oracle["valid"].as_array().unwrap() {
            let text = case["token"].as_str().unwrap();
            let token = Token::decode(text).unwrap();
            assert_eq!(&**token.encode(), text);
            assert_eq!(
                &**Token::decode(case["trimmed_input"].as_str().unwrap())
                    .unwrap()
                    .encode(),
                text
            );
        }
        for case in oracle["invalid"].as_array().unwrap() {
            let text = case["input"].as_str().unwrap();
            let error = Token::decode(text).err().unwrap();
            assert_eq!(
                error,
                match case["error"].as_str().unwrap() {
                    "UnsupportedVersion" => TokenError::UnsupportedVersion,
                    "Malformed" => TokenError::Malformed,
                    other => panic!("unknown independent oracle error class: {other}"),
                }
            );
            assert!(!error.to_string().contains("hww1"));
            assert!(!format!("{error:?}").contains(text));
        }
    }
    #[test]
    fn key_is_stable_atomic_private_and_corruption_is_not_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let root = temporary();
        let first = obtain(&root, false).unwrap();
        let second = obtain(&root, false).unwrap();
        assert_eq!(first.key_id(), second.key_id());
        assert_eq!(first.secret().as_bytes(), second.secret().as_bytes());
        assert_eq!(
            std::fs::metadata(root.join("web/auth.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let rotated = obtain(&root, true).unwrap();
        assert_ne!(first.key_id(), rotated.key_id());
        std::fs::write(root.join("web/auth.key"), b"bad").unwrap();
        assert!(obtain(&root, false).is_err());
        assert!(obtain(&root, true).is_err());
        assert_eq!(std::fs::read(root.join("web/auth.key")).unwrap(), b"bad");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn key_read_requires_exact_unix_0600_without_overwriting_existing_key() {
        use std::os::unix::fs::PermissionsExt;
        let root = temporary();
        let original = obtain(&root, false).unwrap();
        let key = root.join("web/auth.key");
        let bytes = Zeroizing::new(std::fs::read(&key).unwrap());
        for mode in [0o640, 0o644, 0o400, 0o1600] {
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(read(&root).is_err());
            assert!(obtain(&root, false).is_err());
            assert!(obtain(&root, true).is_err());
            assert_eq!(&*std::fs::read(&key).unwrap(), &**bytes);
        }
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let restored = read(&root).unwrap().unwrap();
        assert_eq!(restored.key_id(), original.key_id());
        assert_eq!(restored.secret().as_bytes(), original.secret().as_bytes());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn key_write_failure_distinguishes_preserved_old_key_from_visible_uncertain_commit() {
        let root = temporary();
        let original = obtain(&root, false).unwrap();
        let key = root.join("web/auth.key");
        let bytes = Zeroizing::new(std::fs::read(&key).unwrap());
        let error = with_write_failure(FailurePoint::BeforeRename, || obtain(&root, true))
            .err()
            .unwrap();
        assert!(matches!(error, Error::Persistence(ref error) if !error.commit_visible()));
        assert_eq!(&*std::fs::read(&key).unwrap(), &**bytes);
        assert_eq!(read(&root).unwrap().unwrap().key_id(), original.key_id());
        assert_eq!(
            std::fs::read_dir(root.join("web")).unwrap().count(),
            1,
            "failed temp file removed"
        );
        let error = with_write_failure(FailurePoint::DirectorySync, || obtain(&root, true))
            .err()
            .unwrap();
        assert!(matches!(error, Error::Persistence(ref error) if error.commit_visible()));
        let visible = read(&root).unwrap().unwrap();
        assert_ne!(visible.key_id(), original.key_id());
        assert_ne!(visible.secret().as_bytes(), original.secret().as_bytes());
        assert_ne!(&*std::fs::read(&key).unwrap(), &**bytes);
        assert_eq!(std::fs::read_dir(root.join("web")).unwrap().count(), 1);
        // Injection is scoped to the calling thread and restored even on early unwinding.
        let committed = obtain(&root, true).unwrap();
        assert_eq!(read(&root).unwrap().unwrap().key_id(), committed.key_id());
        std::fs::remove_dir_all(root).unwrap();
    }
}
