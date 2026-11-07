//! 已钉定 TLS 上的单流 Web 管理；不进入设备注册或普通服务分发。
use std::sync::{atomic::Ordering, Arc};

use quinn::{Connection, RecvStream, SendStream, VarInt};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::bridge::{ExitInbound, ManagementRequest};
use super::{FaceCtx, RelayGeneration};
use crate::web;
use crate::{
    ClientConfirmation, ManagementLease, SvcStream, WebClose, WebGrant, WebManagementConfig,
};

struct Held {
    config: WebManagementConfig,
    lease: ManagementLease,
}
impl Held {
    fn take(config: WebManagementConfig, conn_id: u64) -> Option<Self> {
        config
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < web::MAX_SESSIONS).then_some(n + 1)
            })
            .ok()?;
        Some(Self {
            config,
            lease: ManagementLease::new(conn_id),
        })
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        self.lease.cancel();
        self.config.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn close(conn: &Connection, code: WebClose) {
    conn.close(VarInt::from_u32(code as u32), b"web management closed");
}

#[allow(clippy::too_many_arguments)] // 首流所有权和既有连接上下文的唯一移交点。
pub(super) async fn run(
    conn: Connection,
    conn_id: u64,
    ctx: Arc<FaceCtx>,
    generation: Option<RelayGeneration>,
    relay_origin: bool,
    origin_remote: std::net::SocketAddr,
    mut send: SendStream,
    mut recv: RecvStream,
    deadline: Instant,
) {
    if deadline <= Instant::now() {
        close(&conn, WebClose::Deadline);
        return;
    }
    if relay_origin && generation.is_none() {
        close(&conn, WebClose::Relay);
        return;
    }
    let Some(config) = ctx.web_management.clone() else {
        close(&conn, WebClose::Unavailable);
        return;
    };
    let Some(held) = Held::take(config, conn_id) else {
        close(&conn, WebClose::Capacity);
        return;
    };
    let authentication = tokio::time::timeout_at(
        deadline,
        authenticate(
            &conn,
            &ctx,
            &held,
            generation,
            origin_remote,
            &mut send,
            &mut recv,
        ),
    );
    let authenticated = tokio::select! {
        result = authentication => result,
        _ = conn.accept_bi() => { close(&conn, WebClose::Protocol); return; }
    };
    let (grant, pump) = match authenticated {
        Ok(Ok(result)) => result,
        Ok(Err(code)) => {
            close(&conn, code);
            return;
        }
        Err(_) => {
            close(&conn, WebClose::Deadline);
            return;
        }
    };
    #[cfg(unix)]
    let socket = tokio::net::UnixStream::from_std(pump);
    #[cfg(windows)]
    let socket = tokio::net::TcpStream::from_std(pump);
    let Ok(socket) = socket else {
        close(&conn, WebClose::Unavailable);
        return;
    };
    let (mut read, mut write) = tokio::io::split(socket);
    let copying = async {
        tokio::join!(
            super::pump::upstream(&mut recv, &mut write),
            super::pump::downstream(&mut read, &mut send)
        );
    };
    tokio::select! {
        _ = copying => { conn.close(VarInt::from_u32(0), b"web stream ended"); }
        code = invalidated(&grant, &held.lease) => close(&conn, code),
        _ = conn.accept_bi() => close(&conn, WebClose::Protocol),
        _ = conn.closed() => {}
    }
}

async fn invalidated(grant: &WebGrant, lease: &ManagementLease) -> WebClose {
    loop {
        if !grant.is_valid() {
            return WebClose::Revoked;
        }
        if !lease.is_alive() {
            return WebClose::Relay;
        }
        tokio::time::sleep(web::REVOKE_POLL).await;
    }
}

async fn authenticate(
    conn: &Connection,
    ctx: &FaceCtx,
    held: &Held,
    generation: Option<RelayGeneration>,
    origin_remote: std::net::SocketAddr,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> Result<(WebGrant, SvcStream), WebClose> {
    // W1 已由普通准入入口读走，剩余17B仍在同一总期限中。
    let mut hello = [0u8; web::HELLO_LEN - 2];
    recv.read_exact(&mut hello)
        .await
        .map_err(|_| WebClose::Protocol)?;
    if hello[0] != web::VERSION {
        return Err(WebClose::Protocol);
    }
    if !held.config.auth.is_configured() {
        return Err(WebClose::Unavailable);
    }
    let key_id: [u8; 16] = hello[1..].try_into().map_err(|_| WebClose::Protocol)?;
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| WebClose::Unavailable)?;
    let mut challenge = [0u8; web::CHALLENGE_LEN];
    challenge[..3].copy_from_slice(b"WC\x01");
    challenge[3..].copy_from_slice(&nonce);
    send.write_all(&challenge)
        .await
        .map_err(|_| WebClose::Protocol)?;
    let mut exporter = [0u8; 32];
    conn.export_keying_material(&mut exporter, web::EXPORTER_LABEL, b"")
        .map_err(|_| WebClose::Unavailable)?;
    let mut proof = [0u8; web::PROOF_LEN];
    recv.read_exact(&mut proof)
        .await
        .map_err(|_| WebClose::Protocol)?;
    if &proof[..3] != b"WP\x01" {
        return Err(WebClose::Protocol);
    }
    let grant = held
        .config
        .auth
        .verify(&key_id, &nonce, &exporter, &proof)
        .ok_or(WebClose::Authentication)?;
    loop {
        if !grant.is_valid() {
            return Err(WebClose::Revoked);
        }
        let (reply, receive) = oneshot::channel();
        if !ctx
            .bridge
            .push_inbound(ExitInbound::Management(ManagementRequest::new(
                origin_remote,
                generation,
                held.lease.clone(),
                reply,
            )))
        {
            return Err(WebClose::Capacity);
        }
        match receive.await.map_err(|_| WebClose::Unavailable)? {
            ClientConfirmation::Confirmed => break,
            ClientConfirmation::LegMissing => tokio::time::sleep(web::LEG_RETRY).await,
            ClientConfirmation::GenerationMismatch | ClientConfirmation::CapacityFull => {
                return Err(WebClose::Relay)
            }
        }
    }
    if !grant.is_valid() {
        return Err(WebClose::Revoked);
    }
    let (service, pump) = super::pump::socketpair(64 * 1024).map_err(|_| WebClose::Capacity)?;
    held.config
        .intake
        .try_enqueue(service)
        .map_err(|_| WebClose::Capacity)?;
    send.write_all(&web::ACCEPT)
        .await
        .map_err(|_| WebClose::Protocol)?;
    Ok((grant, pump))
}

#[cfg(test)]
#[path = "tests/web.rs"]
mod tests;
