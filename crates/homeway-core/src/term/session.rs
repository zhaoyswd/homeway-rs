//! session — 会话注册表与腿接入语义（R6 6d；PTY/泵/写者接线在 6f）。
//!
//! 行为真源 = baseline 克隆 `pkg/term/{service.go,term_leg.go}` 的注册序与活动选举
//! （design §五「多腿模型」），本模块做**纯状态机**：不持连接/PTY——腿的可见副作用
//! （ENDED 送达目标、尺寸应用、判据行）以返回值交调用方执行。ENDED code/reason 用
//! [`crate::term::frames`] 的冻结词表（只增不改）。
//!
//! 注册序（term_leg.go `registerLegLocked`）：① 同实例替换（ENDED self_reconnect）→
//! ② 显式接管（ENDED replaced）→ ③ 上限腾位（淘汰停滞最久/最久空闲腿，**裸断无 ENDED**）
//! → ④ too_many_clients / 入表 + 接入即活动（选举 → 会话尺寸对齐本腿）。
//! 时间一律由调用方喂（u64 毫秒）——排序真源是单调序号（activitySeq/attachSeq），
//! 墙钟只做淘汰的次级比较（Go 同款：NTP 步进会翻转排序，故不用墙钟做主序）。

use std::collections::HashMap;
use std::time::Duration;

use super::frames::{ended_code, ended_reason, ENDED_REASON_SERVICE_STOPPED};
use super::size::Size;

/// 会话数上限默认值（`HOMEWAY_TERM_MAX_SESSIONS`）。
pub const DEFAULT_MAX_SESSIONS: usize = 16;
/// 每会话腿数上限默认值（`HOMEWAY_TERM_MAX_CLIENTS`）。
pub const DEFAULT_MAX_CLIENTS: usize = 8;

/// ERROR 帧码词表（contract-ledger 台账族③，只增不改；串值与 Go `termErr*` 一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermErrorCode {
    AlreadyExists,
    DetectOff,
    Marshal,
    NoAgent,
    NoSession,
    NoVt,
    SpawnFailed,
    TooMany,
    TooManyClients,
    TermVersion,
}

impl std::fmt::Display for TermErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TermErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            TermErrorCode::AlreadyExists => "already_exists",
            TermErrorCode::DetectOff => "detect_off",
            TermErrorCode::Marshal => "marshal",
            TermErrorCode::NoAgent => "no_agent",
            TermErrorCode::NoSession => "no_session",
            TermErrorCode::NoVt => "no_vt",
            TermErrorCode::SpawnFailed => "spawn_failed",
            TermErrorCode::TooMany => "too_many",
            TermErrorCode::TooManyClients => "too_many_clients",
            TermErrorCode::TermVersion => "term_version",
        }
    }
}

/// 服务面错误（code 词表 + 人类文案；文案与 Go `termErrf` 同串面，CLI 直接呈现）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {msg}")]
pub struct TermError {
    pub code: TermErrorCode,
    pub msg: String,
}

impl TermError {
    pub(crate) fn new(code: TermErrorCode, msg: impl Into<String>) -> Self {
        TermError { code, msg: msg.into() }
    }
}

/// 腿的呈现分类（LIST clients 的 kind 词面；caps 推导，service.go `legKindOf`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegKind {
    /// surface 腿（新 App）。
    App,
    /// 声明 capsRawTerminal 的 raw 腿（CLI）。
    Host,
    /// 未声明的 raw 腿（旧 App）。
    Legacy,
}

impl LegKind {
    pub const fn of(surface: bool, raw_capable: bool) -> LegKind {
        match (surface, raw_capable) {
            (true, _) => LegKind::App,
            (false, true) => LegKind::Host,
            (false, false) => LegKind::Legacy,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            LegKind::App => "app",
            LegKind::Host => "host",
            LegKind::Legacy => "legacy",
        }
    }
}

/// 腿的稳定标识（注册表内单调发号；调用方据它映射到连接/写者）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LegKey(pub u64);

/// 接入一刻的腿描述（HELLO/尾随块解析出的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegDescriptor {
    /// 实例标识（HELLO 尾随 ID 块；空 = 旧客户端）。
    pub client_id: String,
    pub surface: bool,
    pub raw_capable: bool,
    /// 本腿声明的网格尺寸（已归一——类型保证非 0 且在限内）。
    pub size: Size,
}

/// 一条腿的注册态（排序与淘汰所需的全部字段）。
#[derive(Debug, Clone)]
struct LegState {
    key: LegKey,
    client_id: String,
    kind: LegKind,
    size: Size,
    attach_seq: u64,
    last_activity_seq: u64,
    since_ms: u64,
    last_touch_ms: u64,
    removed: bool,
}

/// 被断腿的可见副作用：`ended = None` ⇒ 裸关（不发 ENDED，客户端见裸 EOF）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegEnd {
    pub key: LegKey,
    pub kind: LegKind,
    /// Some((code, reason)) = 要送达的 ENDED；reason 空串合法（自然退出/被 kill）。
    pub ended: Option<(i32, String)>,
    /// 只进日志的归因（client_closed/self_reconnect/takeover/evicted_cap/finish…）。
    pub why: &'static str,
}

/// 接入结果：新腿标识 + 接入引发的可见副作用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterOutcome {
    pub key: LegKey,
    /// 是否首腿（raw 腿握手计划的 focus-in 注入位）。
    pub first: bool,
    /// 被替换/接管/腾位断掉的腿（按触发序）。
    pub ended: Vec<LegEnd>,
    /// 接入即活动 ⇒ 会话尺寸对齐本腿（Some = 尺寸真变，调用方做 PTY setsize + 哨兵）。
    pub size_applied: Option<Size>,
    /// 判据行（腿接入日志，调用方直接打）。
    pub log: String,
}

/// 会话（注册表内的可观测面；PTY/检测态在 6f 并入）。
#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    /// 会话当前尺寸（活动选举改写；类型保证已归一——0×0 不可表达）。
    pub size: Size,
    pub done: bool,
    pub killed: bool,
    /// 子进程退出码（自然退出路径喂入；killed 时 ENDED 用 -2 覆盖）。
    pub exit_code: i32,
    legs: Vec<LegState>,
    attach_seq: u64,
    activity_seq: u64,
    /// 活动腿（尺寸/主题归属；None = 无腿）。
    pub active: Option<LegKey>,
}

impl Session {
    /// 在表腿数。
    pub fn leg_count(&self) -> usize {
        self.legs.iter().filter(|l| !l.removed).count()
    }

    /// 腿清单快照（LIST clients 的呈现面；排序 = 入表序）。
    pub fn legs(&self) -> Vec<LegView> {
        self.legs.iter().filter(|l| !l.removed).map(LegView::of).collect()
    }
}

/// 腿的呈现快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegView {
    pub key: LegKey,
    pub client_id: String,
    pub kind: LegKind,
    pub size: Size,
    pub since_ms: u64,
    pub attach_seq: u64,
}

impl LegView {
    fn of(l: &LegState) -> LegView {
        LegView {
            key: l.key,
            client_id: l.client_id.clone(),
            kind: l.kind,
            size: l.size,
            since_ms: l.since_ms,
            attach_seq: l.attach_seq,
        }
    }
}

/// 会话注册表（服务锁内单线程使用——同 Go「会话锁串行」纪律；6f 接线时持锁调用）。
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<String, Session>,
    max_sessions: usize,
    max_clients: usize,
    next_leg_key: u64,
}

impl SessionRegistry {
    pub fn new(max_sessions: usize, max_clients: usize) -> Self {
        SessionRegistry {
            sessions: HashMap::new(),
            max_sessions: if max_sessions == 0 { DEFAULT_MAX_SESSIONS } else { max_sessions },
            max_clients: if max_clients == 0 { DEFAULT_MAX_CLIENTS } else { max_clients },
            next_leg_key: 1,
        }
    }

    /// HELLO 接入语义（service.go `attachOrCreate`）：不存在 + !create ⇒ no_session；
    /// 不存在 + create + 满 ⇒ too_many；已存在 + create&only_if_absent ⇒
    /// already_exists；其余（`-A` 复用 / 纯 attach）⇒ 复用。**尺寸不在这里动**
    /// （多腿模型下尺寸归活动选举）。
    pub fn attach_or_create(
        &mut self,
        name: &str,
        size: Size,
        create: bool,
        only_if_absent: bool,
    ) -> Result<(), TermError> {
        if !self.sessions.contains_key(name) {
            if !create {
                return Err(TermError::new(TermErrorCode::NoSession, format!("会话 {name} 不存在")));
            }
            if self.sessions.len() >= self.max_sessions {
                return Err(TermError::new(
                    TermErrorCode::TooMany,
                    format!("会话数已达上限 {}，请先关闭一些会话", self.max_sessions),
                ));
            }
            self.sessions.insert(
                name.to_string(),
                Session {
                    name: name.to_string(),
                    size,
                    done: false,
                    killed: false,
                    exit_code: 0,
                    legs: Vec::new(),
                    attach_seq: 0,
                    activity_seq: 0,
                    active: None,
                },
            );
            return Ok(());
        }
        if create && only_if_absent {
            return Err(TermError::new(
                TermErrorCode::AlreadyExists,
                format!("会话 {name} 已存在；要接入请用 attach，或加 -A 复用"),
            ));
        }
        Ok(())
    }

    /// CREATE 不接入（service.go `createOnly`，`new -d`）：不动尺寸、不产生腿、不触发
    /// 哨兵/焦点。尺寸取缺省 80×24（Go `spawnLocked` 的 0 → 80/24 归一；会话几何类型上
    /// 不可为空）。reuse_if_exists 置位 = 存在则静默复用（极性与 HELLO bit1 相反）。
    pub fn create_only(&mut self, name: &str, reuse_if_exists: bool) -> Result<(), TermError> {
        if self.sessions.contains_key(name) {
            if reuse_if_exists {
                return Ok(());
            }
            return Err(TermError::new(
                TermErrorCode::AlreadyExists,
                format!("会话 {name} 已存在；要接入请用 attach，或加 -A 复用"),
            ));
        }
        if self.sessions.len() >= self.max_sessions {
            return Err(TermError::new(
                TermErrorCode::TooMany,
                format!("会话数已达上限 {}，请先关闭一些会话", self.max_sessions),
            ));
        }
        self.sessions.insert(
            name.to_string(),
            Session {
                name: name.to_string(),
                size: Size::DEFAULT,
                done: false,
                killed: false,
                exit_code: 0,
                legs: Vec::new(),
                attach_seq: 0,
                activity_seq: 0,
                active: None,
            },
        );
        Ok(())
    }

    pub fn session(&self, name: &str) -> Option<&Session> {
        self.sessions.get(name)
    }

    pub fn session_mut(&mut self, name: &str) -> Option<&mut Session> {
        self.sessions.get_mut(name)
    }

    pub fn session_names(&self) -> Vec<&str> {
        self.sessions.keys().map(String::as_str).collect()
    }

    /// 注册腿（`registerLegLocked` 全序）。会话已结束 ⇒ no_session；上限满且腾不出位 ⇒
    /// too_many_clients。成功 ⇒ 入表 + 接入即活动（尺寸对齐本腿）。
    ///
    /// `stalled` = 调用方在**同一把服务锁内**从各腿 [`super::legout::LegOut`] 取的实时停滞
    /// 快照（只含停滞腿；本表是淘汰排序的唯一真源——`term_leg.go:927-948` 读实时
    /// `out.isStalled()/stalledFor()` 同义）。注册表保持纯状态机：不做反向回调。
    pub fn register_leg(
        &mut self,
        name: &str,
        desc: LegDescriptor,
        takeover: bool,
        now_ms: u64,
        stalled: &[(LegKey, Duration)],
    ) -> Result<RegisterOutcome, TermError> {
        let max_clients = self.max_clients;
        let next_key = LegKey(self.next_leg_key);
        self.next_leg_key += 1;
        let s = self.sessions.get_mut(name).ok_or_else(|| {
            TermError::new(TermErrorCode::NoSession, format!("会话 {name} 不存在"))
        })?;
        if s.done {
            return Err(TermError::new(TermErrorCode::NoSession, format!("会话 {name} 已结束")));
        }
        let mut out = RegisterOutcome {
            key: next_key,
            first: false,
            ended: Vec::new(),
            size_applied: None,
            log: String::new(),
        };
        // ① 同实例替换（先于接管——同 ID 的旧腿总是先走）
        if !desc.client_id.is_empty() {
            let olds: Vec<LegKey> = s
                .legs
                .iter()
                .filter(|l| !l.removed && l.client_id == desc.client_id)
                .map(|l| l.key)
                .collect();
            for k in olds {
                if let Some(e) = end_leg_internal(s, k, ended_code::REPLACED, ended_reason::SELF_RECONNECT.into(), "self_reconnect") {
                    out.ended.push(e);
                }
            }
        }
        // ② 显式接管（attach -d）
        if takeover {
            let olds: Vec<LegKey> = s.legs.iter().filter(|l| !l.removed).map(|l| l.key).collect();
            for k in olds {
                if let Some(e) = end_leg_internal(s, k, ended_code::REPLACED, ended_reason::REPLACED.into(), "takeover") {
                    out.ended.push(e);
                }
            }
        }
        // ③ 上限腾位（停滞最久优先，其次最久空闲；裸断无 ENDED）
        if s.leg_count() >= max_clients {
            if let Some(victim) = evict_victim(s, stalled) {
                if let Some(e) = end_leg_internal(s, victim, i32::MIN, String::new(), "evicted_cap") {
                    out.ended.push(e);
                }
            }
        }
        if s.leg_count() >= max_clients {
            return Err(TermError::new(
                TermErrorCode::TooManyClients,
                format!("会话 {name} 的客户端腿数已达上限 {max_clients}；可用 -d 显式接管，或先分离其它客户端"),
            ));
        }
        // ④ 入表 + 接入即活动
        out.first = s.leg_count() == 0;
        s.legs.push(LegState {
            key: next_key,
            kind: LegKind::of(desc.surface, desc.raw_capable),
            client_id: desc.client_id.clone(),
            size: desc.size,
            attach_seq: {
                s.attach_seq += 1;
                s.attach_seq
            },
            last_activity_seq: 0,
            since_ms: now_ms,
            last_touch_ms: now_ms,
            removed: false,
        });
        out.size_applied = note_activity_internal(s, next_key, now_ms);
        out.log = format!(
            "term: 会话 {name} 腿接入（kind={} {} id={} 首腿={}）n={}/{}",
            LegKind::of(desc.surface, desc.raw_capable).as_str(),
            desc.size,
            if desc.client_id.is_empty() { "-" } else { &desc.client_id },
            out.first,
            s.leg_count(),
            max_clients,
        );
        Ok(out)
    }

    /// 摘腿（幂等「只摘一次」；code = `ended_code::*` 或子进程退出码；`why` 只进日志）。
    /// code = [i32::MIN] 哨兵 ⇒ 裸关不发 ENDED（同 Go `termEndNone`）。摘腿后的派生
    /// 副作用（afterLegsChangedLocked）一并返回：末腿 ⇒ active 清空 + focus_out 注入位；
    /// 活动腿被摘 ⇒ 重选举（activitySeq 最大、平手按更晚接入；尺寸对齐新活动腿）。
    pub fn end_leg(&mut self, name: &str, key: LegKey, code: i32, reason: &str, why: &'static str, now_ms: u64) -> Option<LegEndOutcome> {
        let s = self.sessions.get_mut(name)?;
        s.legs.iter().find(|l| l.key == key)?;
        let end = end_leg_internal(s, key, code, reason.to_string(), why)?;
        // afterLegsChanged：末腿 → 清 active + focus-out；活动腿被摘 → 重选举
        let mut out = LegEndOutcome { end, re_elected: None, size_applied: None, focus_out: false };
        if s.leg_count() == 0 {
            if s.active.is_some() {
                s.active = None;
            }
            out.focus_out = !s.done; // 末腿离开 → focus-out（TUI 停动画）
        } else if s.active.is_none_or(|a| a == key) {
            let (best, size) = elect_active(s);
            out.re_elected = best;
            out.size_applied = size;
        }
        let _ = now_ms;
        Some(out)
    }

    /// 记一次活动（接入/上报尺寸/输入）。返回会话尺寸应用（Some = 真变，调用方
    /// setsize + 哨兵）。
    pub fn note_activity(&mut self, name: &str, key: LegKey, now_ms: u64) -> Option<Size> {
        let s = self.sessions.get_mut(name)?;
        note_activity_internal(s, key, now_ms)
    }

    /// 腿上报尺寸（RESIZE；活动选举决定是否真改会话尺寸）。入参已归一（[`Size::from_report`]
    /// 的 0 门在调用方）——会话几何只能由合法尺寸写入。
    pub fn leg_resize(&mut self, name: &str, key: LegKey, size: Size) -> Option<Size> {
        let s = self.sessions.get_mut(name)?;
        let l = s.legs.iter_mut().find(|l| l.key == key)?;
        l.size = size;
        None // 尺寸应用归 note_activity（活动选举）——本调用方随后必调 note_activity
    }

    /// 会话收尾（service.go `finish`）：幂等；killed ⇒ code=-2、service_stop ⇒ -3 +
    /// reason 词面、自然退出 ⇒ 退出码 + 空 reason。全腿 ENDED（经调用方写者送达后关 conn）。
    pub fn finish(&mut self, name: &str, reason: FinishReason) -> Vec<LegEnd> {
        let Some(s) = self.sessions.get_mut(name) else {
            return Vec::new();
        };
        if s.done {
            return Vec::new();
        }
        s.done = true;
        let (code, text): (i32, &str) = match reason {
            FinishReason::ServiceStopped => (ended_code::SERVICE_STOPPED, ENDED_REASON_SERVICE_STOPPED),
            FinishReason::Killed => (ended_code::KILLED, ""),
            FinishReason::Exit(c) => (c, ""),
        };
        let keys: Vec<LegKey> = s.legs.iter().filter(|l| !l.removed).map(|l| l.key).collect();
        keys.into_iter()
            .filter_map(|k| end_leg_internal(s, k, code, text.to_string(), "finish"))
            .collect()
    }

    /// kill 的注册面（信号/宽限在 6f）：标记 killed（finish 时 ENDED 用 -2）。
    /// 会话不存在/已结束 ⇒ no_session。
    pub fn kill_mark(&mut self, name: &str) -> Result<(), TermError> {
        let Some(s) = self.sessions.get_mut(name) else {
            return Err(TermError::new(TermErrorCode::NoSession, format!("会话 {name} 不存在")));
        };
        if s.done {
            return Err(TermError::new(TermErrorCode::NoSession, format!("会话 {name} 已结束")));
        }
        s.killed = true;
        Ok(())
    }

    /// 子进程退出码喂入（自然退出路径）。
    pub fn set_exit_code(&mut self, name: &str, code: i32) {
        if let Some(s) = self.sessions.get_mut(name) {
            s.exit_code = code;
        }
    }

    /// 会话收尾后的回收（ENDED 送达/子进程收尸完成后调用；幂等）。
    pub fn remove_session(&mut self, name: &str) -> bool {
        self.sessions.remove(name).is_some()
    }
}

/// 收尾原因（`finish` 的 reason 面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    /// 服务关停（-3 / service_stopped）。
    ServiceStopped,
    /// App KILL（-2；空 reason）。
    Killed,
    /// 子进程自然退出（≥0 退出码；空 reason）。
    Exit(i32),
}

/// 摘腿内部（会话可变借用已持）：幂等位 + 出表 + ENDED 决定。
fn end_leg_internal(s: &mut Session, key: LegKey, code: i32, reason: String, why: &'static str) -> Option<LegEnd> {
    let idx = s.legs.iter().position(|l| l.key == key)?;
    if s.legs[idx].removed {
        return None;
    }
    let leg = &mut s.legs[idx];
    leg.removed = true;
    let ended = (code != i32::MIN).then_some((code, reason));
    Some(LegEnd { key, kind: leg.kind, ended, why })
}

/// 摘腿结果 + 派生副作用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegEndOutcome {
    pub end: LegEnd,
    /// 重选举接管的活动腿（活动腿被摘且有剩余腿时）。
    pub re_elected: Option<LegKey>,
    /// 新活动腿的尺寸应用（Some = 真变，调用方 setsize + 哨兵）。
    pub size_applied: Option<Size>,
    /// 末腿离开的 focus-out 注入位（会话未结束时）。
    pub focus_out: bool,
}

/// 重选举（electActiveLocked）：activitySeq 最大者；平手按更晚接入（attachSeq）优先。
/// 返回（新活动腿, 尺寸应用）。
fn elect_active(s: &mut Session) -> (Option<LegKey>, Option<Size>) {
    let best = s
        .legs
        .iter()
        .filter(|l| !l.removed)
        .max_by(|a, b| (a.last_activity_seq, a.attach_seq).cmp(&(b.last_activity_seq, b.attach_seq)));
    let Some(b) = best else {
        s.active = None;
        return (None, None);
    };
    s.active = Some(b.key);
    let size = if s.size != b.size {
        s.size = b.size;
        Some(b.size)
    } else {
        None
    };
    (s.active, size)
}

/// 活动记账 + 选举（noteActivityLocked / electActiveLocked 的纯逻辑面）：
/// activitySeq++、active 切换、会话尺寸对齐本腿（真变才返回）。会话几何只能由
/// 腿上的 [`Size`] 写入——`RESIZE 0×0` 在入径已被 [`Size::from_report`] 拦下
/// （Go 的会话尺寸写点同样在 0 门之后，F1b）。
fn note_activity_internal(s: &mut Session, key: LegKey, now_ms: u64) -> Option<Size> {
    s.activity_seq += 1;
    let seq = s.activity_seq;
    let l = s.legs.iter_mut().find(|l| l.key == key)?;
    l.last_activity_seq = seq;
    l.last_touch_ms = now_ms;
    let size = l.size;
    s.active = Some(key);
    // 尺寸对齐（applySizeLocked 的真变判定）
    (s.size != size).then(|| {
        s.size = size;
        size
    })
}

/// 腾位受害者：停滞腿里停滞最久者，否则最久空闲腿（evictForSlotLocked）。
/// `stalled` = 调用方锁内取的实时停滞快照（只含停滞腿）——表里查不到 = 未停滞。
fn evict_victim(s: &Session, stalled: &[(LegKey, Duration)]) -> Option<LegKey> {
    let alive: Vec<&LegState> = s.legs.iter().filter(|l| !l.removed).collect();
    let mut victim: Option<(LegKey, Duration)> = None;
    for l in &alive {
        let Some((_, d)) = stalled.iter().find(|(k, _)| *k == l.key) else { continue };
        if victim.is_none() || *d > victim.unwrap().1 {
            victim = Some((l.key, *d));
        }
    }
    if let Some((key, _)) = victim {
        return Some(key);
    }
    let mut idle: Option<&LegState> = None;
    for l in &alive {
        if idle.is_none() || l.last_touch_ms < idle.unwrap().last_touch_ms {
            idle = Some(l);
        }
    }
    idle.map(|l| l.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> SessionRegistry {
        SessionRegistry::new(0, 0) // 默认上限 16/8
    }

    fn sz(cols: u16, rows: u16) -> Size {
        Size::normalized(cols, rows)
    }

    fn desc(id: &str, cols: u16, rows: u16) -> LegDescriptor {
        LegDescriptor {
            client_id: id.to_string(),
            surface: true,
            raw_capable: false,
            size: sz(cols, rows),
        }
    }

    /// attach_or_create 四象限 + createOnly 极性。
    #[test]
    fn attach_or_create_semantics() {
        let mut r = reg();
        // 不存在 + !create
        assert_eq!(
            r.attach_or_create("a", sz(80, 24), false, false).unwrap_err().code,
            TermErrorCode::NoSession
        );
        // 不存在 + create
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        assert_eq!(r.session("a").unwrap().size.cols(), 80);
        // 已存在 + create + only_if_absent
        assert_eq!(
            r.attach_or_create("a", sz(100, 32), true, true).unwrap_err().code,
            TermErrorCode::AlreadyExists
        );
        // 已存在 + 纯 attach（复用，尺寸不动——尺寸归选举）
        r.attach_or_create("a", sz(100, 32), false, false).unwrap();
        assert_eq!(r.session("a").unwrap().size.cols(), 80);
        // CREATE 不接入：极性与 HELLO bit1 相反
        r.create_only("b", false).unwrap();
        assert_eq!(r.session("b").unwrap().leg_count(), 0);
        assert!(r.create_only("b", true).is_ok(), "reuse 置位 = 静默复用");
        assert_eq!(
            r.create_only("b", false).unwrap_err().code,
            TermErrorCode::AlreadyExists
        );
    }

    /// 会话数上限（too_many）。
    #[test]
    fn session_cap() {
        let mut r = SessionRegistry::new(2, 8);
        r.create_only("s1", false).unwrap();
        r.create_only("s2", false).unwrap();
        assert_eq!(
            r.create_only("s3", false).unwrap_err().code,
            TermErrorCode::TooMany
        );
    }

    /// 注册序全链：同实例替换 → 接管 → 腾位 → too_many_clients → 入表即活动。
    #[test]
    fn register_leg_order_and_ended_vocab() {
        let mut r = reg();
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        // 三条腿
        let l1 = r.register_leg("a", desc("id1", 80, 24), false, 100, &[]).unwrap();
        assert!(l1.first && l1.ended.is_empty());
        let l2 = r.register_leg("a", desc("id2", 80, 24), false, 200, &[]).unwrap();
        assert!(!l2.first && l2.ended.is_empty());
        // ① 同实例替换：id2 重连 ⇒ 旧 id2 腿 ENDED(-1, self_reconnect)
        let l3 = r.register_leg("a", desc("id2", 80, 24), false, 300, &[]).unwrap();
        assert_eq!(l3.ended.len(), 1);
        assert_eq!(l3.ended[0].key, l2.key);
        assert_eq!(l3.ended[0].ended, Some((ended_code::REPLACED, ended_reason::SELF_RECONNECT.into())));
        assert_eq!(r.session("a").unwrap().leg_count(), 2);
        // ② 接管：全部旧腿 ENDED(-1, replaced)
        let l4 = r.register_leg("a", desc("id3", 100, 32), true, 400, &[]).unwrap();
        assert_eq!(l4.ended.len(), 2, "接管踢掉全部旧腿");
        assert!(l4.ended.iter().all(|e| e.ended == Some((ended_code::REPLACED, ended_reason::REPLACED.into()))));
        assert_eq!(r.session("a").unwrap().leg_count(), 1);
        // 接入即活动 ⇒ 会话尺寸对齐本腿（100x32）
        assert_eq!(l4.size_applied, Some(sz(100, 32)));
        assert_eq!(r.session("a").unwrap().active, Some(l4.key));
        // 判据行形态
        assert!(l4.log.contains("kind=app 100x32 id=id3 首腿=true）n=1/8"), "判据行（接管踢空后即首腿）：{}", l4.log);
    }

    /// 上限腾位：停滞最久优先、其次最久空闲、腾不出 ⇒ too_many_clients。
    #[test]
    fn evict_prefers_stalled_then_idle() {
        let mut r = SessionRegistry::new(16, 3);
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        let l1 = r.register_leg("a", desc("id1", 80, 24), false, 100, &[]).unwrap().key;
        let l2 = r.register_leg("a", desc("id2", 80, 24), false, 200, &[]).unwrap().key;
        let _l3 = r.register_leg("a", desc("id3", 80, 24), false, 300, &[]).unwrap().key;
        // l2 停滞 5s（其余未停滞）——实时停滞快照随 register_leg 传入（F4：淘汰唯一真源）
        let stalled = [(l2, Duration::from_millis(5000))];
        let l4 = r.register_leg("a", desc("id4", 80, 24), false, 400, &stalled).unwrap();
        assert_eq!(l4.ended.len(), 1);
        assert_eq!(l4.ended[0].key, l2, "停滞腿优先");
        assert!(l4.ended[0].ended.is_none(), "腾位是裸断（无 ENDED——词表没有「被挤出」）");
        assert_eq!(l4.ended[0].why, "evicted_cap");
        // 无停滞 ⇒ 最久空闲（last_touch 最早）
        r.note_activity("a", l1, 900); // l1 刚活动过 ⇒ l3（touch=300）成最旧
        let l5 = r.register_leg("a", desc("id5", 80, 24), false, 1000, &[]).unwrap();
        assert_eq!(l5.ended[0].key, LegKey(3), "无停滞时淘汰最久空闲腿（l3 touch=300 最早）");
        // 腾位总能找到受害者（idle-most 兜底）⇒ 再接一条仍然成功（l4 touch=400 成最旧）；
        // too_many_clients 是防御性死支（victim 只在无腿时为 nil，无腿时上限判定本就不过）
        let l6 = r.register_leg("a", desc("id6", 80, 24), false, 1100, &[]).unwrap();
        assert_eq!(l6.ended[0].key, LegKey(4), "连续腾位：次旧者出局");
        assert_eq!(r.session("a").unwrap().leg_count(), 3);
        let _ = l1;
    }

    /// 活动选举与 tie-break（activitySeq 最大者；平手按更晚接入者优先）。
    #[test]
    fn activity_election_tiebreak() {
        let mut r = reg();
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        let l1 = r.register_leg("a", desc("id1", 80, 24), false, 100, &[]).unwrap().key;
        let l2 = r.register_leg("a", desc("id2", 80, 24), false, 200, &[]).unwrap().key;
        // l2 后接入 ⇒ attach_seq 更大；同活动序（都不活动）下 l2 胜
        assert_eq!(r.session("a").unwrap().active, Some(l2), "接入即活动：后接入者胜");
        // l1 活动一次 ⇒ 序号反超
        r.note_activity("a", l1, 300);
        assert_eq!(r.session("a").unwrap().active, Some(l1));
        // 摘掉活动腿 ⇒ 重选举（activitySeq 最大；l1 已摘 ⇒ l2 接管）
        // 摘掉活动腿 l1 ⇒ 重选举 l2 接管（afterLegsChanged 内建）
        let out = r.end_leg("a", l1, i32::MIN, "", "client_closed", 400).unwrap();
        assert!(out.end.ended.is_none(), "裸断无 ENDED");
        assert_eq!(out.re_elected, Some(l2), "活动腿被摘 ⇒ 重选举");
        assert_eq!(r.session("a").unwrap().active, Some(l2));
        // 末腿离开 ⇒ active 清空 + focus_out 注入位
        let out = r.end_leg("a", l2, i32::MIN, "", "client_closed", 500).unwrap();
        assert_eq!(r.session("a").unwrap().active, None);
        assert!(out.focus_out, "末腿离开注 focus-out（会话未结束）");
    }

    /// 重选举的尺寸对齐（新活动腿尺寸不同 ⇒ 会话尺寸跟随 + 返回应用值）。
    #[test]
    fn re_election_applies_new_active_size() {
        let mut r = reg();
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        let l1 = r.register_leg("a", desc("id1", 80, 24), false, 100, &[]).unwrap().key;
        let _l2 = r.register_leg("a", desc("id2", 100, 32), false, 200, &[]).unwrap().key;
        assert_eq!(r.session("a").unwrap().size.cols(), 100, "后接入的活动腿把会话带到 100x32");
        // l1 上报新尺寸并活动 ⇒ 反超选举 + 尺寸对齐
        r.leg_resize("a", l1, sz(120, 40));
        r.note_activity("a", l1, 300);
        assert_eq!(r.session("a").unwrap().active, Some(l1));
        assert_eq!((r.session("a").unwrap().size.cols(), r.session("a").unwrap().size.rows()), (120, 40));
        // 摘 l1 ⇒ 重选举 _l2（100x32）⇒ 尺寸跟随
        let out = r.end_leg("a", l1, i32::MIN, "", "client_closed", 400).unwrap();
        assert_eq!(out.size_applied, Some(sz(100, 32)));
    }

    /// finish 幂等 + 三种收尾原因的 ENDED 词表。
    #[test]
    fn finish_ended_vocab() {
        let mut r = reg();
        r.attach_or_create("a", sz(80, 24), true, false).unwrap();
        let l1 = r.register_leg("a", desc("id1", 80, 24), false, 100, &[]).unwrap().key;
        let l2 = r.register_leg("a", desc("id2", 80, 24), false, 100, &[]).unwrap().key;
        // 服务关停：全腿 ENDED(-3, service_stopped)
        let ends = r.finish("a", FinishReason::ServiceStopped);
        assert_eq!(ends.len(), 2);
        assert!(ends.iter().all(|e| e.ended == Some((ended_code::SERVICE_STOPPED, ENDED_REASON_SERVICE_STOPPED.into()))));
        assert!(r.session("a").unwrap().done);
        // 幂等
        assert!(r.finish("a", FinishReason::ServiceStopped).is_empty());
        // kill 面：killed 标记 → finish 用 -2；已结束 ⇒ no_session
        r.attach_or_create("b", sz(80, 24), true, false).unwrap();
        r.register_leg("b", desc("id1", 80, 24), false, 100, &[]).unwrap();
        r.kill_mark("b").unwrap();
        let ends = r.finish("b", FinishReason::Killed);
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].ended, Some((ended_code::KILLED, String::new())));
        assert_eq!(r.kill_mark("b").unwrap_err().code, TermErrorCode::NoSession);
        // 自然退出：退出码 ≥0 + 空 reason
        r.attach_or_create("c", sz(80, 24), true, false).unwrap();
        r.register_leg("c", desc("id1", 80, 24), false, 100, &[]).unwrap();
        r.set_exit_code("c", 42);
        let ends = r.finish("c", FinishReason::Exit(42));
        assert_eq!(ends[0].ended, Some((42, String::new())));
        let _ = (l1, l2);
    }

    /// 腿分类词面（caps 推导）。
    #[test]
    fn leg_kind_vocab() {
        assert_eq!(LegKind::of(true, false).as_str(), "app");
        assert_eq!(LegKind::of(true, true).as_str(), "app");
        assert_eq!(LegKind::of(false, true).as_str(), "host");
        assert_eq!(LegKind::of(false, false).as_str(), "legacy");
    }
}
