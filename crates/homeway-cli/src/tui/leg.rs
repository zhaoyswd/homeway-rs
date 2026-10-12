//! 终端腿（tasks 3.4a/b）：接入握手、帧处理、腿满检查与**四分支断线谓词**。
//!
//! 三个纯组件（I/O 循环在 backend 的 app 接线，任务组 4）：
//! - [`build_hello`]：HELLO 构造——flags=0（只接入既有会话，不 CREATE/不接管），
//!   **尾随块携带 caps 块（位按 greeting 声明，可全零）以便 client_id 生效**
//!   （r3 G：`enc_hello_tail` 带 ID 必带 caps 块）；不声明 SURFACE/RAW_TERMINAL
//!   ⇒ 服务端继续代答终端查询（design D2）。
//! - [`TermLeg`]：帧处理器——ATTACHED/DATA/REPLAY_DONE/STATE/ENDED 喂入内嵌
//!   `SessionVt`（出口同款模拟器，r2 N-3 定案）；查询应答收集后**丢弃**
//!   （服务端已代答，客户端转发会造成双重应答）。
//! - [`LegSupervisor`]：四分支谓词（r2 N-1 / spec 需求 5/6）——
//!   ①会话不在 LIST ⇒ 终止停拨；②链路中断 ⇒ 退避 0.5s×2→10s 重拨；
//!   ③腿满 ⇒ 10–30s 待空位；④其余流断 ⇒ 按链路中断。腿上限真源 =
//!   `homeway_core::term::session::DEFAULT_MAX_CLIENTS`（缺省 8；服务端经环境
//!   覆盖为已知限制，服务端接入拒绝为权威兜底，r3 B）。
//!
//! 消费方 = app 主循环的腿线程（任务组 4）——期间豁免 dead_code。

#![allow(dead_code)]

use std::time::Duration;

use homeway_core::term::frames::{self, Frame, Op};
use homeway_core::term::session::DEFAULT_MAX_CLIENTS;
use homeway_core::term::vt::{SessionVt, VtError};

/// 腿上限（预检查口径；真源 = 出口缺省常量 `DEFAULT_MAX_CLIENTS`——r3 B）。
pub(crate) const TERM_LEG_CAP: usize = DEFAULT_MAX_CLIENTS;

// ---------------------------------------------------------------------------
// 接入握手（3.4a）
// ---------------------------------------------------------------------------

/// HELLO 构造（flags=0：接入既有会话；caps 位 = greeting 声明的 PROTO_VER 协商位，
/// **不含** SURFACE/RAW_TERMINAL；caps 块恒在场以承载 client_id——r3 G）。
pub(crate) fn build_hello(name: &str, cols: u16, rows: u16, greeting_feats: u32, client_id: &str) -> Vec<u8> {
    let mut cap_bits = 0u8;
    if greeting_feats & frames::features::PROTO_VER != 0 {
        cap_bits |= frames::caps::PROTO_VER;
    }
    frames::enc_hello(cols, rows, 0, name, &frames::enc_hello_tail(cap_bits, true, client_id))
}

/// 单帧解码（通道字节 → Frame；与 `pull_remote_frame` 的帧形同口径：op + len2LE）。
pub(crate) fn decode_frame(buf: &[u8]) -> Option<Frame> {
    if buf.len() < 3 {
        return None;
    }
    let n = u16::from_le_bytes([buf[1], buf[2]]) as usize;
    if buf.len() < 3 + n {
        return None;
    }
    Some(Frame { op: Op(buf[0]), payload: buf[3..3 + n].to_vec() })
}

/// 接入前检查（spec 需求 5：腿满 MUST NOT 接入；会话不存在 ⇒ 终止语义）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Precheck {
    Attach,
    Full,
    Missing,
}

pub(crate) fn precheck(exists: bool, legs: usize) -> Precheck {
    if !exists {
        Precheck::Missing
    } else if legs >= TERM_LEG_CAP {
        Precheck::Full
    } else {
        Precheck::Attach
    }
}

// ---------------------------------------------------------------------------
// 帧处理（3.4a）
// ---------------------------------------------------------------------------

/// 腿帧处理结果（调用方据此驱动重绘与状态行）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LegTick {
    Attached,
    ReplayDone,
    StateUpdated,
    Terminated,
    Fed,
}

/// 腿处理器：内嵌 `SessionVt`（出口同款）+ 握手/状态簿记。
pub(crate) struct TermLeg {
    vt: SessionVt,
    /// ATTACHED 的会话几何（与会话尺寸对齐校验用）。
    pub attached_size: Option<(u16, u16)>,
    /// STATE 推送的最新 agent/stateV2/title（选中会话的徽章/标题实况）。
    pub state: Option<(u8, u8, String)>,
    pub replay_done: bool,
    pub replay_truncated: bool,
    /// ENDED（code, reason）——服务端明示终止（四分支①的前置信号）。
    pub ended: Option<(i32, String)>,
}

impl TermLeg {
    pub fn new(cols: u16, rows: u16) -> Result<Self, VtError> {
        Ok(TermLeg {
            vt: SessionVt::new(cols, rows, 0)?, // scrollback=0 ⇒ 落到真源缺省
            attached_size: None,
            state: None,
            replay_done: false,
            replay_truncated: false,
            ended: None,
        })
    }

    /// 喂入一帧。未知 op（NOTIFY 等 surface 族）忽略——raw 腿协议面不携带。
    pub fn on_frame(&mut self, f: &Frame) -> LegTick {
        match f.op {
            Op::ATTACHED => {
                if let Ok((cols, rows, _modes, _agent, _state, _name)) = frames::dec_attached(&f.payload) {
                    self.attached_size = Some((cols, rows));
                }
                LegTick::Attached
            }
            Op::DATA => {
                // 服务端已代答查询（本腿不声明 RAW_TERMINAL ⇒ 让位规则不触发）：
                // 收集到的应答在此丢弃，绝不回写 PTY（双重应答会毒化会话）。
                self.vt.write_collecting(&f.payload, &mut |_| {});
                LegTick::Fed
            }
            Op::REPLAY_DONE => {
                let Ok((_replayed, flags)) = frames::dec_replay_done(&f.payload) else { return LegTick::Fed; };
                self.replay_done = true;
                self.replay_truncated = flags & frames::replay_flags::TRUNCATED != 0;
                LegTick::ReplayDone
            }
            Op::STATE => {
                let (agent, state, title) = frames::dec_state(&f.payload);
                self.state = Some((agent, state, title));
                LegTick::StateUpdated
            }
            Op::ENDED => {
                let (code, reason) = frames::dec_ended(&f.payload);
                self.ended = Some((code, reason));
                LegTick::Terminated
            }
            _ => LegTick::Fed,
        }
    }

    /// 屏幕内容整体取用（UI 渲染入口；`rows_and_commit` 同时清脏标——合帧渲染）。
    pub fn rows_and_commit(&mut self) -> Vec<homeway_core::term::vt::Row> {
        self.vt.rows_and_commit()
    }

    pub fn cursor(&mut self) -> homeway_core::term::vt::Cursor {
        self.vt.cursor()
    }

    pub fn size(&self) -> (u16, u16) {
        self.vt.size()
    }

    /// 本地 VT 跟尺寸（r5 实录：窗口 resize 只改了会话侧 PTY——本地网格不跟随
    /// ⇒ 重炮应用按新尺寸重画时整屏折行错位/残留）。随 RESIZE 下发同拍调用。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.vt.resize(cols, rows);
    }

    /// 会话模式（括号粘贴/焦点上报/鼠标上报位——输入编码的门，spec 需求 4）。
    pub fn modes(&self) -> homeway_core::term::vt::Modes {
        self.vt.modes()
    }

    /// 键编码口径快照（kitty flags 等）。
    pub fn key_options(&self) -> homeway_core::term::keyenc::KeyOptions {
        self.vt.key_options()
    }

    /// 内嵌模拟器访问（输入编码按会话模式位取口径——runtime 透传路径用）。
    pub fn vt(&self) -> &homeway_core::term::vt::SessionVt {
        &self.vt
    }

    // —— 滚轮回看（tasks 4.8；core 滚动面 = design D7 同路径最小公开）——
    /// 滚动偏移（0 = 贴底）。
    pub fn display_offset(&self) -> usize {
        self.vt.display_offset()
    }

    /// 滚轮驱动滚动（±delta，贴顶/贴底截断）。
    pub fn scroll_display(&mut self, delta: i32) {
        self.vt.scroll_display(delta);
    }

    /// 按当前偏移取行（回看瞬态视图；offset=0 由调用方走 rows_and_commit）。
    pub fn rows_at_offset(&self) -> Vec<homeway_core::term::vt::Row> {
        self.vt.rows_at_offset()
    }
}

// ---------------------------------------------------------------------------
// 四分支断线谓词（3.4b）
// ---------------------------------------------------------------------------

/// 腿监督相位。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LegPhase {
    /// 已接入/观察中。
    Watching,
    /// 裸断待判定（流断后等待调用方拉 LIST + 链路状态）。
    Lost,
    /// 会话终止（终态：停拨；「会话已结束」展示与回落由 app 层执行）。
    Ended,
    /// 链路中断：退避重拨。
    LinkDown { next: Duration },
    /// 腿满：待空位（恢复条件 = LIST 见空位，spec 需求 5）。
    WaitVacancy { next: Duration },
}

/// 监督器事件（I/O 侧把 LIST 拍/链路状态/流终结翻译成这些）。
#[derive(Debug)]
pub(crate) enum LegEvent {
    /// 接入前检查。
    Precheck { exists: bool, legs: usize },
    /// 流终结。`ended = Some` = 收到 ENDED 帧（服务端明示终止，不经 LIST）。
    StreamEnd { ended: bool },
    /// 裸断后的判定拍（谓词的输入三信号：会话存在性/链路/腿满）。
    Judge { exists: bool, legs: usize, link_up: bool },
    /// 链路恢复信号（控制面事件/本机 socket 恢复）。
    LinkUp,
    /// WaitVacancy 期间的占用复查拍。
    VacancyPoll { legs: usize },
}

/// 监督器决策。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LegAction {
    Attach,
    Redial { delay: Duration },
    WaitVacancy { delay: Duration },
    /// 停拨（会话已结束）。
    Stopped,
    /// 无动作（事件与本相位无关）。
    None,
}

const LINK_BACKOFF_INIT: Duration = Duration::from_millis(500);
const LINK_BACKOFF_MAX: Duration = Duration::from_secs(10);
const VACANCY_INIT: Duration = Duration::from_secs(10);
const VACANCY_MAX: Duration = Duration::from_secs(30);

/// 腿监督器：四分支谓词的状态面。退避在「成功接入」时重置（spec：恢复即重置）。
#[derive(Debug)]
pub(crate) struct LegSupervisor {
    phase: LegPhase,
}

impl LegSupervisor {
    pub fn new() -> Self {
        LegSupervisor { phase: LegPhase::Watching }
    }

    pub fn phase(&self) -> &LegPhase {
        &self.phase
    }

    pub fn on(&mut self, ev: LegEvent) -> LegAction {
        match ev {
            LegEvent::Precheck { exists, legs } => match precheck(exists, legs) {
                Precheck::Missing => {
                    self.phase = LegPhase::Ended;
                    LegAction::Stopped
                }
                Precheck::Full => {
                    self.phase = LegPhase::WaitVacancy { next: VACANCY_INIT };
                    LegAction::WaitVacancy { delay: VACANCY_INIT }
                }
                Precheck::Attach => {
                    self.phase = LegPhase::Watching;
                    LegAction::Attach
                }
            },
            LegEvent::StreamEnd { ended } => {
                // ENDED 帧 = 服务端明示终止（①的前置信号，不经 LIST 即可判）。
                if ended {
                    self.phase = LegPhase::Ended;
                    return LegAction::Stopped;
                }
                if matches!(self.phase, LegPhase::Watching) {
                    self.phase = LegPhase::Lost;
                }
                LegAction::None
            }
            LegEvent::Judge { exists, legs, link_up } => {
                if matches!(self.phase, LegPhase::Ended) {
                    return LegAction::Stopped;
                }
                // 谓词依序：①会话不存在 ⇒ 终止；②链路中断；③腿满；④其余按链路中断。
                if !exists {
                    self.phase = LegPhase::Ended;
                    return LegAction::Stopped;
                }
                if !link_up {
                    let next = match &self.phase {
                        LegPhase::LinkDown { next } => (*next * 2).min(LINK_BACKOFF_MAX),
                        _ => LINK_BACKOFF_INIT,
                    };
                    self.phase = LegPhase::LinkDown { next };
                    return LegAction::Redial { delay: next };
                }
                if legs >= TERM_LEG_CAP {
                    self.phase = LegPhase::WaitVacancy { next: VACANCY_INIT };
                    return LegAction::WaitVacancy { delay: VACANCY_INIT };
                }
                // ④ 其余流断：按链路中断（沿用既有退避节拍——连续失败倍增）。
                let next = match &self.phase {
                    LegPhase::LinkDown { next } => (*next * 2).min(LINK_BACKOFF_MAX),
                    _ => LINK_BACKOFF_INIT,
                };
                self.phase = LegPhase::LinkDown { next };
                LegAction::Redial { delay: next }
            }
            LegEvent::LinkUp => {
                // 链路恢复：退避重置并立即重附（spec：链路恢复事件立即重置）。
                if matches!(self.phase, LegPhase::LinkDown { .. } | LegPhase::Lost) {
                    self.phase = LegPhase::Watching;
                    return LegAction::Attach;
                }
                LegAction::None
            }
            LegEvent::VacancyPoll { legs } => match &self.phase {
                LegPhase::WaitVacancy { next } => {
                    if legs < TERM_LEG_CAP {
                        self.phase = LegPhase::Watching;
                        LegAction::Attach
                    } else {
                        let next = (*next * 2).min(VACANCY_MAX);
                        self.phase = LegPhase::WaitVacancy { next };
                        LegAction::WaitVacancy { delay: next }
                    }
                }
                _ => LegAction::None,
            },
        }
    }
}

impl Default for LegSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeway_core::term::frames::{caps, features, replay_flags};

    // ---- HELLO 字节级契约（r3 G/B） -------------------------------------

    #[test]
    fn hello_carries_caps_block_so_client_id_survives() {
        let hello = build_hello("main", 120, 40, features::PROTO_VER, "tui-abc");
        let (_cols, _rows, flags, name, tail) = frames::dec_hello(&hello).expect("HELLO 可解码");
        assert_eq!(flags, 0, "不 CREATE、不接管——只接入既有会话");
        assert_eq!(name, "main");
        let t = frames::dec_hello_tail(tail).expect("尾随块可解码");
        assert!(t.caps_present, "caps 块恒在场（caps_present=false 会静默丢 client_id，r3 G）");
        assert_eq!(t.caps, caps::PROTO_VER, "仅声明协商位；不声明 SURFACE/RAW_TERMINAL（服务端继续代答）");
        assert_eq!(t.client_id, "tui-abc", "client_id 完整到达");
    }

    #[test]
    fn hello_without_proto_ver_still_carries_empty_caps_block() {
        let hello = build_hello("s", 80, 24, 0, "id-1");
        let t = frames::dec_hello_tail(frames::dec_hello(&hello).unwrap().4).unwrap();
        assert!(t.caps_present);
        assert_eq!(t.caps, 0, "greeting 未声明协商位 ⇒ 全零 caps 块");
        assert_eq!(t.client_id, "id-1");
    }

    // ---- 腿满检查（r3 B） -----------------------------------------------

    #[test]
    fn precheck_matrix() {
        assert_eq!(precheck(true, 3), Precheck::Attach);
        assert_eq!(precheck(true, TERM_LEG_CAP), Precheck::Full, "腿满（缺省 8，真源 DEFAULT_MAX_CLIENTS）");
        assert_eq!(precheck(true, TERM_LEG_CAP + 1), Precheck::Full);
        assert_eq!(precheck(false, 0), Precheck::Missing);
    }

    // ---- 帧处理 ----------------------------------------------------------

    fn frame(op: Op, payload: Vec<u8>) -> Frame {
        Frame { op, payload }
    }

    #[test]
    fn leg_frames_drive_state_and_termination() {
        let mut leg = TermLeg::new(80, 24).expect("建腿");
        assert!(!leg.replay_done);
        // ATTACHED
        let att = frames::enc_attached(120, 40, 0, 1, 2, "main");
        assert_eq!(leg.on_frame(&frame(Op::ATTACHED, att)), LegTick::Attached);
        assert_eq!(leg.attached_size, Some((120, 40)));
        // DATA 进网格（清屏前序 + 文本，服务端回放同序）
        let data = b"\x1b[2J\x1b[Hhello leg".to_vec();
        assert_eq!(leg.on_frame(&frame(Op::DATA, data)), LegTick::Fed);
        // REPLAY_DONE（带截断位）
        let rd = frames::enc_replay_done(123, replay_flags::TRUNCATED);
        assert_eq!(leg.on_frame(&frame(Op::REPLAY_DONE, rd)), LegTick::ReplayDone);
        assert!(leg.replay_done && leg.replay_truncated);
        // STATE 推送（agent/stateV2/title）
        let st = frames::enc_state(frames::agent::CODEX, frames::state_v2::BLOCKED, "重构 auth");
        assert_eq!(leg.on_frame(&frame(Op::STATE, st)), LegTick::StateUpdated);
        assert_eq!(leg.state, Some((frames::agent::CODEX, frames::state_v2::BLOCKED, "重构 auth".into())));
        // ENDED ⇒ 终止
        let ended = frames::enc_ended(0, "user");
        assert_eq!(leg.on_frame(&frame(Op::ENDED, ended)), LegTick::Terminated);
        assert_eq!(leg.ended, Some((0, "user".into())));
    }

    #[test]
    fn leg_modes_track_bracketed_and_focus_from_stream() {
        // 内嵌模拟器自己从字节流跟踪 2004/1004（输入编码的门，spec 需求 4）。
        let mut leg = TermLeg::new(80, 24).expect("建腿");
        assert!(!leg.modes().bracketed_paste && !leg.modes().focus_events);
        leg.on_frame(&frame(Op::DATA, b"\x1b[?2004h\x1b[?1004h".to_vec()));
        let m = leg.modes();
        assert!(m.bracketed_paste && m.focus_events);
    }

    // ---- 四分支谓词（r2 N-1 / r3 E/F 语义） ------------------------------

    #[test]
    fn predicate_full_matrix() {
        let mut s = LegSupervisor::new();
        // 接入前检查
        assert_eq!(s.on(LegEvent::Precheck { exists: true, legs: 2 }), LegAction::Attach);
        assert_eq!(s.phase(), &LegPhase::Watching);
        // ④ 其余流断（会话在、链路好、腿未满）⇒ 按链路中断重附
        assert_eq!(s.on(LegEvent::StreamEnd { ended: false }), LegAction::None);
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 3, link_up: true }), LegAction::Redial { delay: LINK_BACKOFF_INIT });
        // ② 链路中断（link_up=false）⇒ 沿用倍增节拍（连续失败不重置）
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 3, link_up: false }), LegAction::Redial { delay: Duration::from_secs(1) });
        // ③ 腿满 ⇒ 待空位（10s 起）
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 8, link_up: true }), LegAction::WaitVacancy { delay: VACANCY_INIT });
        // ① 会话不存在 ⇒ 终止停拨
        assert_eq!(s.on(LegEvent::Judge { exists: false, legs: 0, link_up: true }), LegAction::Stopped);
        assert_eq!(s.phase(), &LegPhase::Ended);
    }

    #[test]
    fn ended_frame_stops_without_list() {
        let mut s = LegSupervisor::new();
        assert_eq!(s.on(LegEvent::Precheck { exists: true, legs: 1 }), LegAction::Attach);
        assert_eq!(s.on(LegEvent::StreamEnd { ended: true }), LegAction::Stopped, "ENDED = 服务端明示终止，不经 LIST");
        assert_eq!(s.phase(), &LegPhase::Ended);
        // 终态粘滞：后续事件不改判。
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 0, link_up: true }), LegAction::Stopped);
    }

    #[test]
    fn link_backoff_doubles_and_resets_on_link_up() {
        let mut s = LegSupervisor::new();
        assert_eq!(s.on(LegEvent::Precheck { exists: true, legs: 1 }), LegAction::Attach);
        assert_eq!(s.on(LegEvent::StreamEnd { ended: false }), LegAction::None);
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 1, link_up: false }), LegAction::Redial { delay: Duration::from_millis(500) });
        // 连续失败倍增（重拨仍失败 ⇒ 再 Judge）。
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 1, link_up: false }), LegAction::Redial { delay: Duration::from_secs(1) });
        assert_eq!(s.on(LegEvent::Judge { exists: true, legs: 1, link_up: false }), LegAction::Redial { delay: Duration::from_secs(2) });
        // 链路恢复 ⇒ 退避重置、立即重附。
        assert_eq!(s.on(LegEvent::LinkUp), LegAction::Attach);
        assert_eq!(s.phase(), &LegPhase::Watching);
    }

    #[test]
    fn vacancy_backoff_doubles_to_cap_and_attach_on_vacancy() {
        let mut s = LegSupervisor::new();
        assert_eq!(s.on(LegEvent::Precheck { exists: true, legs: 8 }), LegAction::WaitVacancy { delay: VACANCY_INIT });
        // 空位未现：10s → 20s → 30s（封顶）。
        assert_eq!(s.on(LegEvent::VacancyPoll { legs: 8 }), LegAction::WaitVacancy { delay: Duration::from_secs(20) });
        assert_eq!(s.on(LegEvent::VacancyPoll { legs: 8 }), LegAction::WaitVacancy { delay: Duration::from_secs(30) });
        assert_eq!(s.on(LegEvent::VacancyPoll { legs: 8 }), LegAction::WaitVacancy { delay: Duration::from_secs(30) });
        // 空位出现 ⇒ 接入。
        assert_eq!(s.on(LegEvent::VacancyPoll { legs: 2 }), LegAction::Attach);
        assert_eq!(s.phase(), &LegPhase::Watching);
    }
}
