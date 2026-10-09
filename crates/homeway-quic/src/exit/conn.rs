//! 每连接两枚任务（出口面 · `current_thread` runtime 内）：
//!
//! - [`control`]：首条 bidi 控制流 = `hr-reg4` **四帧准入**（Hello→Challenge→Proof→Accept），
//!   绑定之后同一控制流转入 60s 刷新帧（`R4`）循环；
//! - [`datagrams`]：DATAGRAM 收包 → 绑定查表 → **源校验** → 入队 + 唤醒（§1.4）。
//!
//! 发送面（[`send_datagram_checked`]）是本 crate **唯一**的 `Connection::send_datagram`
//! 调用点：裸调用在缓冲满时会静默淘汰最旧且恒返 Ok（M1 设计 §0.3 P4 实测），故全部
//! 出口包走「预检 + 分类计数 + 才发」。
//!
//! 单线程前提（§6.4）：本 crate 的 runtime 是 `current_thread`，且预检与发送之间**无
//! `await`** ⇒ 同线程上不存在「预检通过后、发送前」被他处插入的空间竞争（这也是
//! `datagram_send_buffer_space()` 两次加锁读可用的前提）。
//!
//! **准入定序（设计 §1.4 的单一漏斗）**——本文件是它的实现落点：
//!
//! ```text
//! 0) 连接被采纳：起 ADMIT_DEADLINE 计时（到点未走完 ⇒ 弃 + 关连接）
//! 1) 帧可解？否 ⇒ 拒（版本不符 `H2`/`H3` 与垃圾包**可辨**，r14 F4）
//! 2) Hello ⇒ 生成 nonce、写 C4、置 pending（**不触碰设备表、不投引擎**）
//! 3) Proof ⇒ nonce 一次性消费（take 先于任何后续判定）+ 窗内？ ⇒ 投引擎裁决
//! 4) 裁决 Accepted ⇒ bind（E-q2 行）⇒ 写 A4；Rejected{why} ⇒ 按 why 打归因行 + 关连接
//! 5) 绑定之后：只接受 R4（已绑定连接拒 H4/P4，r14 F15）；刷新**不重绑、不打 E-q2**（r14 F11）
//! ```
//!
//! 「未触碰设备表」= 步骤 1/2/3 的拒绝路径（设备表 `entries`、`rej` 计数、判据行都不动）
//! ——这正是「未认证连接不占额度」的结构性落点（设计 §2.2）。

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use quinn::{Connection, SendDatagramError, VarInt};
use tokio::sync::oneshot;

use crate::reg4::{
    self, AcceptFrame, ChallengeFrame, FrameHead, HelloFrame, Nonce, ProofFrame, RefreshFrame,
    Reg4Frame,
};
use crate::sync_util::lock_unpoison;

use super::admit::{PROOF_FAIL_COOLDOWN, PROOF_FAIL_WINDOW};
use super::bridge::{
    Bound, DropKind, EngineRejectClass, ExitInbound, Reg4Request, Reg4Verdict, RejectWhy, dev_short,
};
use super::{FaceCtx, log_due};

/// 发送结果（测试面：`Dropped` 与计数一一对应）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SendOutcome {
    Sent,
    Dropped,
}

/// 入站内层包的源校验（**复刻** `homeway-core` 的 `server/device.rs` 的 `src_allowed`，
/// M1 设计 §1.3/§1.4；**M3 §6 收窄**）：非 IPv4（短包或版本 ≠ 4）或 `src != tun_ip` ⇒ 拒。
///
/// 为什么是复刻而不是调用：QUIC 入站**不经过** `device.rs`（本 crate 是叶子，不得依赖
/// `homeway-core`）；行为必须逐字同（拒绝计数对应今日的 `src_rejects`）。
///
/// **收窄（M3 设计 §6；登记 = §8.2-6）**：接受集从 `{tunnel_ip, tun_ip}` → **`{tun_ip}`**。
/// 理由：QUIC 档的内层包源恒为 **App 的 TUN 地址**（`hw-app` 派生地址，E-q2 行
/// `tun=` 字段与绑定表同源），而 `tunnel_ip`（`hw-tun` 派生、栈 B 的地址）在 QUIC 档
/// **没有合法来源**（栈 B 的数据面不在 QUIC 档上）⇒ 保留它只扩大可伪造面。
/// 方向 = **安全面收紧**；`Bound.tunnel_ip` 字段按设计**保留**（不再参与源校验，
/// 「值域/输入集变化」已登记）。
fn src_allowed(pkt: &[u8], tun_ip: Ipv4Addr) -> bool {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return false;
    }
    let src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    src == tun_ip
}

/// 出站内层包 → DATAGRAM：预检（`max_datagram_size` / 发送缓冲空余）⇒ 分类计数 ⇒ 才发。
pub(crate) fn send_datagram_checked(conn: &Connection, pkt: Vec<u8>, ctx: &FaceCtx) -> SendOutcome {
    match conn.max_datagram_size() {
        // 连接未就绪（对端尚未确认 DATAGRAM 参数）⇒ 归 `未登记`（§6.4 的映射表）
        None => {
            ctx.bridge
                .note_drop(DropKind::Unregistered, "连接未就绪（max_datagram_size 未知）");
            return SendOutcome::Dropped;
        }
        Some(mds) if pkt.len() > mds => {
            ctx.bridge.note_drop(
                DropKind::TooLarge,
                &format!("包 {}B > max_datagram_size {mds}B", pkt.len()),
            );
            return SendOutcome::Dropped;
        }
        Some(_) => {}
    }
    let space = conn.datagram_send_buffer_space();
    if space < pkt.len() {
        // 裸 `send_datagram` 在这个点会静默淘汰最旧并返 Ok（P4）——这里丢 + 计数（不静默）
        ctx.bridge.note_drop(
            DropKind::SendBufferFull,
            &format!("缓冲空余 {space}B < 包 {}B（不静默：丢 + 计数）", pkt.len()),
        );
        return SendOutcome::Dropped;
    }
    match conn.send_datagram(Bytes::from(pkt)) {
        Ok(()) => SendOutcome::Sent,
        Err(SendDatagramError::TooLarge) => {
            ctx.bridge
                .note_drop(DropKind::TooLarge, "quinn TooLarge（本地单报文上限）");
            SendOutcome::Dropped
        }
        // UnsupportedByPeer / Disabled / ConnectionLost ⇒ `未登记`（§6.4 按变体映射）
        Err(e) => {
            ctx.bridge.note_drop(DropKind::Unregistered, &format!("发送失败（{e}）"));
            SendOutcome::Dropped
        }
    }
}

/// 准入期的单连接临时状态（**任务内局部变量**：无共享表、无锁、无 `Arc`）。
///
/// 「连接 = 设备」的未认证面天然按连接隔离 ⇒ 状态上界 = 连接数上界（构造性有界），
/// 不需要淘汰策略（设计 §1.3）。
struct Pending {
    nonce: Nonce,
    issued_at: Instant,
}

/// 控制流任务：`hr-reg4` 四帧准入 ⇒ 绑定 ⇒ 同一控制流转入刷新帧循环。
///
/// 帧是**定长**的（`reg4` 的长度常量）：按「读 2B 魔数 → 定长收全帧」切分；任何一帧被拒
/// ⇒ 关连接（准入是连接级的，拒绝后不给「同连接再试」的口子）。
pub(crate) async fn control(conn: Connection, conn_id: u64, ctx: Arc<FaceCtx>) {
    // 阶段 ①：准入（有界）——`ADMIT_DEADLINE` **包住「等首帧」（`accept_bi`）+ 整段状态机**。
    // 控制流两半边的存放：`admit` 拿到后填进 `streams`，超时被 drop 时只是没人取（无害）。
    let deadline = ctx.admit_deadline;
    let mut streams: Option<(quinn::SendStream, quinn::RecvStream)> = None;
    match bounded_by(deadline, admit(&conn, conn_id, &ctx, &mut streams)).await {
        Some(Admitted::Bound) => {}
        Some(Admitted::Rejected) => return, // 已拒 + 已关连接
        None => {
            // 「握手完成但不发 Hello」（连控制流都不开）与「Hello 之后不发 Proof」到点 ⇒
            // 本期限收口（r14 F3：`max_idle_timeout` + `keep_alive` 组合下这类连接**不会
            // 自然过期**，M1 的握手闸只管 `Connecting` 阶段 ⇒ 必须由本期限兜住）。
            time_out(&ctx, &conn, deadline, TimeoutKind::AdmitTimeout);
            return;
        }
    }
    let Some((send, mut recv)) = streams else { return }; // `Bound` 必有流；防御位
    // 发送半边保持打开（不写、不 reset）：提前 drop 会给对端发 RESET_STREAM（无谓噪声）。
    let _keepalive_send = send;
    // 阶段 ②：已绑定连接的刷新循环（**无期限**——空闲是常态，最长等 max_idle_timeout）
    // **+ M3 §2.1 的服务流受理循环**（同任务 `join!`，不新增任务/线程）。
    //
    // 为什么受理必须**在准入之后**才起：`hr-reg4` 把**首条 bidi 流**固定为控制流，
    // 准入的 `accept_bi` 与受理循环的 `accept_bi` 并发 = 两条消费者抢同一条流
    // （受理赢了就把控制流吃掉 ⇒ 准入永久卡住）。`join!` 的形态同时保证：连接结束
    // （刷新循环 `read_head` 出错返回）时受理循环随 accept 失败退出，两个都收。
    let serve = super::serve::serve_streams(conn.clone(), conn_id, Arc::clone(&ctx));
    tokio::join!(refresh_loop(&conn, conn_id, &ctx, &mut recv), serve);
}

/// 准入阶段的期限包装（设计 §1.3 的 `ADMIT_DEADLINE`）：**包住「等首帧（`accept_bi`）+
/// 整段状态机」**，`None` = 到点。
///
/// 抽成具名函数的理由：纯定时语义要能被 `start_paused` 虚拟时钟**确定性**断言（挂在
/// `control()` 里的匿名 `timeout` 形态在不建真实连接时构造不出来），且期限与
/// `max_idle_timeout` 的关系（前者必须更紧，否则「握手完成但不发 Hello」永不回收）在同处可查。
pub(super) async fn bounded_by<F: std::future::Future>(
    deadline: Duration,
    fut: F,
) -> Option<F::Output> {
    tokio::time::timeout(deadline, fut).await.ok()
}

/// 准入阶段的结果（`Bound` ⇒ 调用方转刷新循环；`Rejected` ⇒ 连接已关，收摊）。
enum Admitted {
    Bound,
    Rejected,
}

/// 阶段 ①：四帧准入（**已被 `ADMIT_DEADLINE` 包住**，含等首帧的 `accept_bi`）。
async fn admit(
    conn: &Connection,
    conn_id: u64,
    ctx: &FaceCtx,
    streams: &mut Option<(quinn::SendStream, quinn::RecvStream)>,
) -> Admitted {
    // 等首帧（开控制流）也在期限内：不开流 = 本期限到点被回收（r14 F3）
    let Ok(pair) = conn.accept_bi().await else {
        return Admitted::Rejected; // 连接在开流前就没了
    };
    *streams = Some(pair);
    let Some((send, recv)) = streams.as_mut() else {
        return Admitted::Rejected;
    };
    // ---- 首帧：必须是 Hello（其余一切 ⇒ 拒）----
    let Some((hbytes, head)) = read_head(recv).await else {
        return Admitted::Rejected; // 对端关流/连接死（无「拒绝」语义，不计拒绝）
    };
    if let Some(why) = head.legacy_why() {
        reject(ctx, conn, &[0u8; 8], why, Counter::BeforeChallenge, CloseCode::BadData);
        return Admitted::Rejected;
    }
    match head {
        FrameHead::Hello => {}
        FrameHead::Refresh => {
            // 未绑定连接上的刷新帧（设计 §1.4 步骤 6 的前置①）
            reject(
                ctx,
                conn,
                &[0u8; 8],
                "刷新帧但连接未绑定",
                Counter::BeforeChallenge,
                CloseCode::BadData,
            );
            return Admitted::Rejected;
        }
        _ => {
            reject(
                ctx,
                conn,
                &[0u8; 8],
                "帧格式非法（首帧必须是 Hello）",
                Counter::BeforeChallenge,
                CloseCode::BadData,
            );
            return Admitted::Rejected;
        }
    }
    let mut hbuf = [0u8; reg4::HELLO_LEN];
    debug_assert_eq!(Some(hbuf.len()), head.len(), "定长切帧的唯一来源 = FrameHead::len()");
    if !read_frame(recv, hbytes, &mut hbuf).await {
        return Admitted::Rejected;
    }
    let Some(hello) = HelloFrame::parse(&hbuf) else {
        reject(
            ctx,
            conn,
            &[0u8; 8],
            "帧格式非法（魔数/长度）",
            Counter::BeforeChallenge,
            CloseCode::BadData,
        );
        return Admitted::Rejected;
    };

    // ---- 证明失败闸（§3.2-⑥）：冷却期内**不再发 Challenge**（拒 Hello，r14 F12）----
    {
        let cooling = lock_unpoison(&ctx.proof_gate).is_cooling(Instant::now(), hello.dev_tag);
        if cooling {
            reject(
                ctx,
                conn,
                &hello.dev_tag,
                "证明失败闸冷却中（同 dev 短时多次 nonce/MAC 类失败——暂不发挑战）",
                Counter::BeforeChallenge,
                CloseCode::Credential,
            );
            return Admitted::Rejected;
        }
    }

    // ---- 生成 nonce + 写 Challenge（**不触碰设备表、不投引擎**）----
    let nonce = match Nonce::generate() {
        Ok(n) => n,
        Err(e) => {
            // 随机源不可用 = 本出口已不可信：拒（不降级成弱新鲜值）
            reject(
                ctx,
                conn,
                &hello.dev_tag,
                &format!("挑战不可发（{e}）"),
                Counter::BeforeChallenge,
                CloseCode::Resource,
            );
            return Admitted::Rejected;
        }
    };
    if send.write_all(&ChallengeFrame::encode(&nonce)).await.is_err() {
        return Admitted::Rejected; // 写不出去 = 连接坏；挑战没发出 ⇒ 不计挑战
    }
    let n = ctx.stats.challenges_issued.fetch_add(1, Ordering::SeqCst) + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 准入挑战已发（{}；在途未认证 {}/{}；第 {n} 次）",
            conn.remote_address(),
            inflight_unauthenticated(ctx),
            ctx.conn_cap
        ));
    }
    let pending = Pending { nonce, issued_at: Instant::now() };

    // ---- 第二帧：必须是 Proof，且必须在 NONCE_TTL 内到达 ----
    let waited = match tokio::time::timeout(ctx.nonce_ttl, read_head(recv)).await {
        Ok(Some(h)) => h,
        Ok(None) => return Admitted::Rejected, // 连接死
        Err(_elapsed) => {
            // pending 过期（设计 §1.3 的「过期处置」：拒 + 记行 + 关连接，不等 idle 30s）
            time_out(ctx, conn, ctx.nonce_ttl, TimeoutKind::PendingExpired);
            return Admitted::Rejected;
        }
    };
    let (pbytes, phead) = waited;
    if let Some(why) = phead.legacy_why() {
        reject(
            ctx,
            conn,
            &hello.dev_tag,
            why,
            Counter::AtProof,
            CloseCode::BadData,
        );
        return Admitted::Rejected;
    }
    if phead != FrameHead::Proof {
        // 重复 Hello（r14 F22：一个连接只接受一个 Hello）/ 任何非 Proof 帧
        let why = if phead == FrameHead::Hello {
            "重复 Hello（一个连接只接受一个 Hello）"
        } else {
            "帧格式非法（Hello 之后必须是 Proof）"
        };
        reject(
            ctx,
            conn,
            &hello.dev_tag,
            why,
            Counter::BeforeChallenge,
            CloseCode::BadData,
        );
        return Admitted::Rejected;
    }
    let mut pbuf = [0u8; reg4::PROOF_LEN];
    debug_assert_eq!(Some(pbuf.len()), phead.len(), "定长切帧的唯一来源 = FrameHead::len()");
    if !read_frame(recv, pbytes, &mut pbuf).await {
        return Admitted::Rejected;
    }
    let Some(proof) = ProofFrame::parse(&pbuf) else {
        reject(
            ctx,
            conn,
            &hello.dev_tag,
            "帧格式非法（魔数/长度）",
            Counter::AtProof,
            CloseCode::BadData,
        );
        return Admitted::Rejected;
    };

    // ---- nonce：**一次性消费**（本帧即 `pending` 的唯一消费者）且必须在窗内 ----
    // 任务串行 ⇒ 不存在并发窗口；「已消费」的第二帧只会看到连接已关（先拒后关）。
    let ok_nonce =
        proof.nonce.ct_eq(&pending.nonce) && pending.issued_at.elapsed() <= ctx.nonce_ttl;
    if !ok_nonce {
        note_proof_fail(ctx, &proof.dev_tag); // §3.2-⑥：nonce 类失败进证明失败闸
        reject(
            ctx,
            conn,
            &proof.dev_tag,
            "nonce 缺失/过期/已消费",
            Counter::AtProof,
            CloseCode::Credential,
        );
        return Admitted::Rejected;
    }

    // ---- 投引擎裁决（MAC 试秘 + 设备表原路径）----
    let Some(exporter) = exporter_of(conn) else {
        // TLS exporter 取不到 = 该连接不是可绑定形态（理论上 TLS1.3 后必可得）
        reject(
            ctx,
            conn,
            &proof.dev_tag,
            "TLS exporter 不可得",
            Counter::AtProof,
            CloseCode::BadData,
        );
        return Admitted::Rejected;
    };
    let Some(verdict) = ask_engine(ctx, conn, Reg4Frame::Proof(proof), exporter).await else {
        return Admitted::Rejected; // 引擎侧未回执（收工）：连接留给收工链
    };
    match verdict {
        Reg4Verdict::Accepted { tunnel_ip, tun_ip } => {
            // 绑定（E-q2 采纳行在 bind 内）⇒ **绑定建立之后立刻**写 A4（设计 §1.2）
            ctx.stats.regs_accepted.fetch_add(1, Ordering::SeqCst);
            ctx.bridge
                .bind(proof.dev_tag, proof.pubkey, conn_id, conn.clone(), tunnel_ip, tun_ip);
            if send.write_all(&AcceptFrame::encode()).await.is_err() {
                // 回执写不出去 = 客户端会超时重连（绑定仍在，由其重登记覆盖）
                return Admitted::Rejected;
            }
            Admitted::Bound
        }
        Reg4Verdict::Rejected { why } => {
            // §3.2-⑥ 的计数集 = nonce/MAC 类；**引擎裁决拒绝（表满/冲突/吊销/窗超）
            // 一律不计数**（r14 F12：可用性故障不得被放大成冷却锁死）
            if why == RejectWhy::MacMismatch {
                note_proof_fail(ctx, &proof.dev_tag);
            }
            reject(
                ctx,
                conn,
                &proof.dev_tag,
                why.text(),
                Counter::AtProof,
                CloseCode::of_why(why),
            );
            Admitted::Rejected
        }
    }
}

/// 记一次 **nonce/MAC 类**失败（设计 §3.2-⑥ 的计数集；调用点 = nonce 判负与
/// `MacMismatch` 裁决两处，引擎裁决拒绝不在此列）。
///
/// 跨过阈值时打「证明失败闸」行（行文照 §3.3 的可观测行；计数与快照 `proof_cooldowns`
/// 同源）。本函数**只在准入路径**被调用：冷却的效果是「不再发 Challenge」，故输入也只取
/// 准入面的 nonce/MAC 类失败（刷新路径的 MAC 失败不计——避免把一次刷新抖动放大成准入锁死）。
fn note_proof_fail(ctx: &FaceCtx, dev: &[u8; 8]) {
    let entered = lock_unpoison(&ctx.proof_gate).note_fail(Instant::now(), *dev);
    let Some(count) = entered else { return };
    let n = ctx.stats.proof_cooldowns.fetch_add(1, Ordering::SeqCst) + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 证明失败闸（dev={} 在 {PROOF_FAIL_WINDOW:?} 内失败 {count} 次——冷却 {PROOF_FAIL_COOLDOWN:?}）",
            dev_short(dev)
        ));
    }
}

/// 阶段 ②：已绑定连接的控制流（**只接受刷新帧**；`H4`/`P4` 一律拒——r14 F15）。
///
/// 刷新**不重绑、不打 E-q2**（r14 F11）：设备表侧走 `Refreshed` 路径，出口面的绑定原样
/// 在位；C15' 行由客户端面打（行文不变，M1 已登记）。
///
/// **前置两道（零设备表查询）**：①连接已在本连接号上绑定 ②帧内 `pub/devTag` == 绑定。
/// 第三道「表内仍在册」（设计 §1.4 步骤 6 ② / r14 F25）需要设备表 ⇒ 在**引擎裁决**里判
/// （`admit_reg4` 的 `Refresh` 分支，MAC 试秘之前），本条只负责把拒绝的 `why` 打出来 + 关连接。
async fn refresh_loop(conn: &Connection, conn_id: u64, ctx: &FaceCtx, recv: &mut quinn::RecvStream) {
    let mut buf = [0u8; reg4::REFRESH_LEN];
    loop {
        let Some((hbytes, head)) = read_head(recv).await else {
            return; // 对端关流/连接死
        };
        let Some(bound) = ctx.bridge.binding_of_conn(conn_id) else {
            // 绑定已摘（设备被摘除/轮换/连接被替换）：拒绝 + 收摊（不再有「已绑定」身份）
            if head.is_client_inbound() {
                reject(
                    ctx,
                    conn,
                    &[0u8; 8],
                    "连接未绑定（绑定已摘）",
                    Counter::BeforeChallenge,
                    CloseCode::Session,
                );
            }
            return;
        };
        if let Some(why) = head.legacy_why() {
            reject(
                ctx,
                conn,
                &bound.dev,
                why,
                Counter::BeforeChallenge,
                CloseCode::Session,
            );
            return;
        }
        match head {
            FrameHead::Refresh => {
                debug_assert_eq!(Some(buf.len()), head.len(), "定长切帧的唯一来源 = FrameHead::len()");
                if !read_frame(recv, hbytes, &mut buf).await {
                    return;
                }
                let Some(frame) = RefreshFrame::parse(&buf) else {
                    reject(
                        ctx,
                        conn,
                        &bound.dev,
                        "帧格式非法（魔数/长度）",
                        Counter::BeforeChallenge,
                        CloseCode::Session,
                    );
                    return;
                };
                if frame.pubkey != bound.pubkey || frame.dev_tag != bound.dev {
                    reject(
                        ctx,
                        conn,
                        &bound.dev,
                        "刷新帧与绑定身份不符",
                        Counter::BeforeChallenge,
                        CloseCode::Session,
                    );
                    return;
                }
                let Some(exporter) = exporter_of(conn) else {
                    reject(
                        ctx,
                        conn,
                        &bound.dev,
                        "TLS exporter 不可得",
                        Counter::BeforeChallenge,
                        CloseCode::Session,
                    );
                    return;
                };
                let Some(verdict) = ask_engine(ctx, conn, Reg4Frame::Refresh(frame), exporter).await
                else {
                    return; // 引擎未回执（收工）
                };
                match verdict {
                    // **刷新成功：不重绑、不打 E-q2 行**（r14 F11）——表内 `Refreshed` 已延长
                    // `last_reg`，出口面绑定与派生地址原样在位。
                    Reg4Verdict::Accepted { .. } => {
                        ctx.stats.regs_accepted.fetch_add(1, Ordering::SeqCst);
                    }
                    Reg4Verdict::Rejected { why } => {
                        reject(
                            ctx,
                            conn,
                            &bound.dev,
                            why.text(),
                            Counter::BeforeChallenge,
                            CloseCode::Session,
                        );
                        return;
                    }
                }
            }
            // **已绑定连接的再准入**（r14 F15）：否则 `bind()` 会把本连接改指到另一个 dev，
            // 旧 dev 的 `by_dev`/`by_pub` 残留 ⇒「连接 = 设备」破裂。
            FrameHead::Hello | FrameHead::Proof => {
                reject(
                    ctx,
                    conn,
                    &bound.dev,
                    "已绑定连接的再准入",
                    Counter::BeforeChallenge,
                    CloseCode::Session,
                );
                return;
            }
            _ => {
                reject(
                    ctx,
                    conn,
                    &bound.dev,
                    "帧格式非法（已绑定连接只收刷新帧）",
                    Counter::BeforeChallenge,
                    CloseCode::Session,
                );
                return;
            }
        }
    }
}

/// 读 2B 魔数（返**原始字节 + 判别**；流读失败 ⇒ `None`：连接死/对端关流，**不是**拒绝）。
async fn read_head(recv: &mut quinn::RecvStream) -> Option<([u8; 2], FrameHead)> {
    let mut head = [0u8; 2];
    recv.read_exact(&mut head).await.ok()?;
    Some((head, FrameHead::of(head)))
}

/// 读满一帧（首 2B 已在 [`read_head`] 里读走——**必须写回 `buf[..2]`**，否则解帧恒失败）。
async fn read_frame(recv: &mut quinn::RecvStream, head: [u8; 2], buf: &mut [u8]) -> bool {
    buf[..2].copy_from_slice(&head);
    recv.read_exact(&mut buf[2..]).await.is_ok()
}

/// 取本连接的 TLS exporter（连接绑定值）。
fn exporter_of(conn: &Connection) -> Option<[u8; reg4::EXPORTER_LEN]> {
    let mut exporter = [0u8; reg4::EXPORTER_LEN];
    conn.export_keying_material(&mut exporter, reg4::EXPORTER_LABEL, b"")
        .ok()?;
    Some(exporter)
}

/// 投引擎并等回执（`None` = 引擎侧未回执：收工路径）。
async fn ask_engine(
    ctx: &FaceCtx,
    conn: &Connection,
    frame: Reg4Frame,
    exporter: [u8; reg4::EXPORTER_LEN],
) -> Option<Reg4Verdict> {
    let (reply_tx, reply_rx) = oneshot::channel();
    if !ctx
        .bridge
        .push_inbound(ExitInbound::Reg(Reg4Request::new(frame, exporter, reply_tx)))
    {
        ctx.bridge
            .note_drop(DropKind::Unregistered, "准入请求（入境队列满 8192 条）");
        // M3 §4：入境队列满 = 资源桶（0x12）——准入窗内的第三处带码点
        conn.close(
            VarInt::from_u32(crate::admit_close::code::RESOURCE as u32),
            b"inbound queue full",
        );
        return None;
    }
    reply_rx.await.ok()
}

/// 「在途未认证」= 存活连接数 − 已绑定设备数（设计 §1.6 挑战行的计数面）。
fn inflight_unauthenticated(ctx: &FaceCtx) -> u64 {
    let alive = ctx.stats.connections.load(Ordering::SeqCst);
    let bound = ctx.bridge.bound_count() as u64;
    alive.saturating_sub(bound)
}

/// 准入关闭码的**分派面**（M3 §4：出口写进 `CONNECTION_CLOSE` 的 `VarInt`）。
///
/// 取值单源 = [`crate::admit_close::code`]（客户端读同一份表）；本枚举只承担
/// 「哪一处拒绝归哪一桶」的映射，**不另写码值**。
///
/// **哪些点带码**（设计 §4 的「4 + 3 处」表）：只有**准入窗内**的三处
/// （`reject()` / `time_out()` / 入境队列满）；准入后的会话级关闭（替换/摘除）与
/// 刷新面拒绝留 [`CloseCode::Session`]（`0`）——客户端据此不把它们误报成「准入被拒」。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CloseCode {
    /// `0x11` 凭证不被接受（MAC 不符 / 证明失败闸 / 引擎 `no-token`·`revoked`）。
    Credential,
    /// `0x12` 资源暂不可用（引擎 `table-full`·表压 / 入境队列满）。
    Resource,
    /// `0x13` 准入数据非法（帧格式/版本族）。
    BadData,
    /// `0x14` 准入超时（两族计时器）。
    Timeout,
    /// 会话级关闭（**非准入面**）：码值恒 `0`（= 今天的行为）。
    Session,
}

impl CloseCode {
    fn raw(self) -> u64 {
        use crate::admit_close::code as c;
        match self {
            CloseCode::Credential => c::CREDENTIAL,
            CloseCode::Resource => c::RESOURCE,
            CloseCode::BadData => c::BAD_DATA,
            CloseCode::Timeout => c::TIMEOUT,
            CloseCode::Session => 0,
        }
    }

    /// 引擎裁决拒绝 → 桶（§4 的两桶表；`RefreshNotRegistered` 是刷新面 ⇒ 会话级）。
    fn of_why(why: RejectWhy) -> CloseCode {
        match why {
            RejectWhy::MacMismatch => CloseCode::Credential,
            RejectWhy::EngineRejected { class } => match class {
                EngineRejectClass::Credential => CloseCode::Credential,
                EngineRejectClass::Resource => CloseCode::Resource,
            },
            RejectWhy::RefreshNotRegistered => CloseCode::Session,
        }
    }
}

/// 拒绝计数的落点（**准入漏斗的两段**——两段之和 = `regs_rejected`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Counter {
    /// 未发挑战的拒绝（帧非法/版本不符/未绑定刷新/重复 Hello/身份不符/刷新被拒）。
    BeforeChallenge,
    /// Proof 阶段的拒绝（nonce 类/MAC 类/引擎裁决拒绝/exporter 不可得）。
    AtProof,
}

impl Counter {
    fn bump(self, ctx: &FaceCtx) {
        match self {
            Self::BeforeChallenge => &ctx.stats.challenges_refused,
            Self::AtProof => &ctx.stats.proof_rejected,
        }
        .fetch_add(1, Ordering::SeqCst);
    }
}

/// 关连接 + 计数 + 记行（**准入拒绝的唯一出口**）。
///
/// **M3 §4**：`code` 决定 `CONNECTION_CLOSE` 的应用码（准入窗内三处带码；刷新/会话级
/// 拒绝留 `Session` = `0`）。行文与计数集**逐字不变**（出口侧详细归因行不改）。
fn reject(ctx: &FaceCtx, conn: &Connection, dev: &[u8; 8], why: &str, at: Counter, code: CloseCode) {
    at.bump(ctx);
    let n = ctx.stats.regs_rejected.fetch_add(1, Ordering::SeqCst) + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 准入被拒（dev={} ← {}；{why}；第 {n} 次）",
            dev_short(dev),
            conn.remote_address()
        ));
    }
    conn.close(VarInt::from_u32(code.raw() as u32), b"registration rejected");
}

/// 期限到点（`ADMIT_DEADLINE` / `NONCE_TTL`）：弃连接 + 计数 + 节流记行。
///
/// 与 [`reject`] 分开的理由（设计 §1.6 的行族）：超时不是「对端发了坏帧」，归因行不同
/// （`认证超时`），且**不等** `max_idle_timeout=30s`——主动 `CONNECTION_CLOSE`。
/// **M3 §4**：两族计时器一律 `0x14`（准入超时；客户端据此把「出口没答应」与「出口拒了」
/// 分开）。
fn time_out(ctx: &FaceCtx, conn: &Connection, dur: Duration, which: TimeoutKind) {
    let n = match which {
        TimeoutKind::AdmitTimeout => ctx.stats.admit_timeouts.fetch_add(1, Ordering::SeqCst),
        TimeoutKind::PendingExpired => ctx.stats.pending_expired.fetch_add(1, Ordering::SeqCst),
    } + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 认证超时（{} 未在 {dur:?} 内完成证明——已弃；第 {n} 次）",
            conn.remote_address()
        ));
    }
    conn.close(
        VarInt::from_u32(CloseCode::Timeout.raw() as u32),
        b"admission timeout",
    );
}

/// [`time_out`] 的两类计时器（各自独立计数）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TimeoutKind {
    /// 准入总期限（连接被采纳起算）。
    AdmitTimeout,
    /// pending 过期（Challenge 发出起算）。
    PendingExpired,
}

/// 数据报任务：`read_datagram` → 绑定查表 → 源校验 → 入队 + 唤醒。
///
/// **未认证连接的门禁在此**（设计 §2.2-1 的结构性落点）：绑定不在位 ⇒ 数据报**丢 + 计数**，
/// 不触碰引擎、不占任何设备表额度（M1 已落，M2 把它接到 `hr-reg4` 的裁决面上）。
pub(crate) async fn datagrams(conn: Connection, conn_id: u64, ctx: Arc<FaceCtx>) {
    loop {
        let dg = match conn.read_datagram().await {
            Ok(d) => d,
            Err(_) => return, // 连接死/被替换
        };
        // 绑定查表（§1.3）：未登记连接的数据报**直接丢 + 计数**（含被替换的旧连接）
        // （M3 §6 收窄后 `tunnel_ip` 不再参与判定——避免为读取它而引入无谓的解构）
        let Some(Bound { dev, tun_ip, .. }) = ctx.bridge.binding_of_conn(conn_id) else {
            ctx.bridge
                .note_drop(DropKind::Unregistered, "未登记连接的数据报");
            continue;
        };
        // 源校验（复刻 `device.rs` 的 `src_allowed`；§1.3/§1.4；M3 §6 收窄为单元素集）
        if !src_allowed(&dg, tun_ip) {
            ctx.bridge.note_drop(
                DropKind::SrcRejected,
                &format!(
                    "src={} ∉ {{tun_ip={tun_ip}}}（dev={}）",
                    src_text(&dg),
                    dev_short(&dev)
                ),
            );
            continue;
        }
        if !ctx
            .bridge
            .push_inbound(ExitInbound::Packet { dev, pkt: dg.to_vec() })
        {
            ctx.bridge.note_drop(DropKind::Unregistered, "入境队列满（8192 条）");
        }
    }
}

/// 内层包的源地址文案（E-q3 明细：M1 真机发现①「源校验拒 ≥3 次无法定性」⇒ 补实际 `src`，
/// 设计 §9.1-1 / r14 F21 的**可选登记条**；主字段行文不变）。
fn src_text(pkt: &[u8]) -> String {
    if pkt.len() >= 20 && pkt[0] >> 4 == 4 {
        Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]).to_string()
    } else {
        format!("非 IPv4（{}B）", pkt.len())
    }
}
