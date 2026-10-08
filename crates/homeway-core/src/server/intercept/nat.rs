//! 包级 NAT 重写 + RST/ICMP responder 构造（R3 拦截层的纯函数面）。
//!
//! smoltcp 无 gVisor 的 promiscuous/spoofing/任意端口动态 accept——「终结过境 TCP +
//! 本机重拨」的替代 = 用户态 REDIRECT：入站包按五元组映射把 dst 重写为
//! `(拦截栈地址, rw_port)`（校验和重算），出站应答把 src 反重写回**原始目的**
//! （客户端看到的源/目地址与 Go（gVisor spoof 端点）完全一致）。

use std::net::Ipv4Addr;

/// 入站明文 IPv4 包的可视图（借用零拷贝——各字段校验后引用原包）。
pub struct Ipv4View<'a> {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub proto: u8,
    pub src_port: u16,
    pub dst_port: u16,
    pub header_len: usize,
    pub total_len: usize,
    pub payload: &'a [u8],
    pub tcp_flags: u8,
    pub tcp_seq: u32,
    pub tcp_ack: u32,
    /// TCP 头 window 字段原始 u16（未 shift——R8-4 8n 归因观测面）。
    pub tcp_win: u16,
    /// 分片偏移（IP 头 flags 低 13 位；首片 = 0）。
    pub frag_off: u16,
    /// MF（More Fragments，IP 头 flags bit 13）。
    pub mf: bool,
}

pub const TCP_SYN: u8 = 0x02;
pub const TCP_ACK: u8 = 0x10;
pub const TCP_RST: u8 = 0x04;
pub const TCP_FIN: u8 = 0x01;

impl<'a> Ipv4View<'a> {
    /// 解析 IPv4 + TCP/UDP 伪头部字段；非法（长度/版本/截断）返回 None。
    pub fn parse(pkt: &'a [u8]) -> Option<Self> {
        if pkt.len() < 20 || pkt[0] >> 4 != 4 {
            return None;
        }
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        if ihl < 20 || pkt.len() < ihl {
            return None;
        }
        let total_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
        if total_len < ihl || total_len > pkt.len() {
            return None;
        }
        let proto = pkt[9];
        let src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
        let dst = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);
        // 分片字段（bytes 6-7：flags 高 3 位 + 分片偏移低 13 位——F7）
        let flags_frag = u16::from_be_bytes([pkt[6], pkt[7]]);
        let mf = flags_frag & 0x2000 != 0;
        let frag_off = flags_frag & 0x1FFF;
        let body = &pkt[ihl..total_len];
        let mut payload_off = 0usize;
        let (src_port, dst_port, tcp_flags, tcp_seq, tcp_ack, tcp_win) = match proto {
            6 => {
                if body.len() < 20 {
                    return None;
                }
                // TCP 载荷起点按 data offset（options 不算载荷——RST 的 seq 推进语义）
                payload_off = ((body[12] >> 4) as usize).max(5) * 4;
                (
                    u16::from_be_bytes([body[0], body[1]]),
                    u16::from_be_bytes([body[2], body[3]]),
                    body[13],
                    u32::from_be_bytes(body[4..8].try_into().unwrap()),
                    u32::from_be_bytes(body[8..12].try_into().unwrap()),
                    u16::from_be_bytes([body[14], body[15]]),
                )
            }
            17 => {
                if body.len() < 8 {
                    return None;
                }
                (u16::from_be_bytes([body[0], body[1]]), u16::from_be_bytes([body[2], body[3]]), 0, 0, 0, 0)
            }
            _ => (0, 0, 0, 0, 0, 0),
        };
        let l4_hdr = if proto == 6 {
            payload_off
        } else if proto == 17 {
            8
        } else {
            0
        };
        Some(Self {
            src,
            dst,
            proto,
            src_port,
            dst_port,
            header_len: ihl,
            total_len,
            payload: &body[l4_hdr.min(body.len())..],
            tcp_flags,
            tcp_seq,
            tcp_ack,
            tcp_win,
            frag_off,
            mf,
        })
    }

    /// 分片判定（F7）：非首片（`frag_off > 0`）或 MF 置位。DF-only（flags=0x4000、
    /// off=0、MF=0）**不**算分片——照常处理。
    pub fn is_fragment(&self) -> bool {
        self.frag_off != 0 || self.mf
    }

    /// 过境五元组键（UDP 会话表与 TCP 映射共用形态）。
    pub fn five_tuple(&self) -> (Ipv4Addr, u16, Ipv4Addr, u16) {
        (self.src, self.src_port, self.dst, self.dst_port)
    }

    pub fn is_tcp_syn(&self) -> bool {
        self.proto == 6 && self.tcp_flags & TCP_SYN != 0 && self.tcp_flags & TCP_ACK == 0
    }
}

/// 轻量 IPv4 头视图（Q-K F1.2；**只解 IP 头，不碰 L4**——分片判定不应依赖 L4 可解析性）。
///
/// 与 `Ipv4View::parse` 的区别：`Ipv4View` 对「非首片且 body < 8」的合法小分片返回
/// `None`，会把它们当畸形静默丢而非走分片路径（`fragDrop` 不涨）；本视图只做 IP 头
/// 层面的校验，供分片门（RX 重组 / TX 反重写）使用。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ipv4FragHdr {
    pub src: Ipv4Addr,
    pub dst: Ipv4Addr,
    pub proto: u8,
    /// IPv4 identification（重组键之一）。
    pub ident: u16,
    /// IP 头长度（含选项；20..=60，已校验）。
    pub header_len: usize,
    /// 本包声明的总长（含 IP 头；已校验 ≤ `pkt.len()`）。
    pub total_len: usize,
    /// 分片偏移（IP 头 flags 低 13 位；**字节偏移 = 本值 × 8**）。
    pub frag_off: u16,
    /// MF（More Fragments，IP 头 flags bit 13）。
    pub mf: bool,
}

/// 一包的分片视图：IP 头 + **本片载荷**（Q-K F1.2 —— 类型承担不变量：头与载荷
/// 恒来自同一包，签名层面杜绝「头视图 + 整包」不自洽的调用）。
#[derive(Clone, Copy, Debug)]
pub struct FragSlice<'a> {
    pub hdr: Ipv4FragHdr,
    /// IP 头原始字节 `pkt[..header_len]`（含选项；`len() == hdr.header_len`）——
    /// 重组时作为整包头的模板。
    pub head: &'a [u8],
    /// `pkt[header_len .. total_len]`——**尾随字节丢弃**（不是 `pkt[ihl..]`：
    /// 否则链路填充会被夹带进重组结果）。
    pub payload: &'a [u8],
}

impl Ipv4FragHdr {
    /// 只解 IP 头 + 一次取片。校验集与 `Ipv4View::parse` 同口径（任一不满足 ⇒ None）：
    /// `pkt.len() >= 20`；version == 4；`ihl = (pkt[0]&0x0f)*4` 且 `20 <= ihl <= 60`；
    /// `pkt.len() >= ihl`；`total_len >= ihl`；`total_len <= pkt.len()`。
    pub fn parse(pkt: &[u8]) -> Option<FragSlice<'_>> {
        if pkt.len() < 20 || pkt[0] >> 4 != 4 {
            return None;
        }
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        if !(20..=60).contains(&ihl) || pkt.len() < ihl {
            return None;
        }
        let total_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
        if total_len < ihl || total_len > pkt.len() {
            return None;
        }
        let flags_frag = u16::from_be_bytes([pkt[6], pkt[7]]);
        let hdr = Ipv4FragHdr {
            src: Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]),
            dst: Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]),
            proto: pkt[9],
            ident: u16::from_be_bytes([pkt[4], pkt[5]]),
            header_len: ihl,
            total_len,
            frag_off: flags_frag & 0x1FFF,
            mf: flags_frag & 0x2000 != 0,
        };
        Some(FragSlice { hdr, head: &pkt[..ihl], payload: &pkt[ihl..total_len] })
    }

    /// 分片判定：非首片（`frag_off > 0`）或 MF 置位。DF-only（flags=0x4000、off=0、
    /// MF=0）**不**算分片（与 `Ipv4View::is_fragment` 同式、同源字节 `pkt[6..8]`）。
    pub fn is_fragment(&self) -> bool {
        self.frag_off != 0 || self.mf
    }
}

/// Internet 校验和的**未取反**折叠和（RFC1071；多段联用：各段结果相加再折叠一次）。
fn checksum_raw(data: &[u8], mut sum: u32) -> u32 {
    let mut i = 0;
    while i + 1 < data.len() {
        sum = sum.wrapping_add(u16::from_be_bytes([data[i], data[i + 1]]) as u32);
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    sum
}

/// 单段 Internet 校验和（取反一次——RFC1071 终值）。
fn checksum(data: &[u8], sum: u32) -> u16 {
    !(checksum_raw(data, sum) as u16)
}

/// IPv4 头校验和重算（原地：清零 [10..12] 后算）。
pub(crate) fn fix_ip_checksum(pkt: &mut [u8]) {
    if pkt.len() < 20 {
        return;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    pkt[10] = 0;
    pkt[11] = 0;
    let sum = checksum(&pkt[..ihl.min(pkt.len())], 0);
    pkt[10..12].copy_from_slice(&sum.to_be_bytes());
}

/// L4 校验和重算（TCP/UDP，伪头部含源/目 IP——改了任一端都必须重算）。
/// UDP 校验和结果 0x0000 时按规范写 0xFFFF（0 表示「无校验」）。
fn fix_l4_checksum(pkt: &mut [u8]) {
    if pkt.len() < 20 {
        return;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    let proto = pkt[9];
    if pkt.len() <= ihl {
        return;
    }
    let l4_len = pkt.len() - ihl;
    let pseudo = [
        pkt[12], pkt[13], pkt[14], pkt[15], // src
        pkt[16], pkt[17], pkt[18], pkt[19], // dst
        0,
        proto,
        (l4_len >> 8) as u8,
        l4_len as u8,
    ];
    let mut sum = checksum_raw(&pseudo, 0);
    let l4 = &mut pkt[ihl..];
    match proto {
        6 if l4.len() >= 18 => {
            l4[16] = 0;
            l4[17] = 0;
            sum = checksum_raw(l4, sum);
            let c = !(sum as u16); // 伪头部 + 段联合折叠后取反一次
            l4[16..18].copy_from_slice(&c.to_be_bytes());
        }
        17 if l4.len() >= 6 => {
            l4[6] = 0;
            l4[7] = 0;
            sum = checksum_raw(l4, sum);
            let c0 = !(sum as u16);
            let c = if c0 == 0 { 0xFFFF } else { c0 }; // UDP 0 表示无校验
            l4[6..8].copy_from_slice(&c.to_be_bytes());
        }
        _ => {}
    }
}

/// 重写目的地址（NAT 正向：dst → (拦截栈地址, rw_port)）。返回可变整包。
pub fn rewrite_dst(pkt: &mut [u8], new_ip: Ipv4Addr, new_port: u16) {
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    pkt[16..20].copy_from_slice(&new_ip.octets());
    match pkt[9] {
        6 => pkt[ihl + 2..ihl + 4].copy_from_slice(&new_port.to_be_bytes()),
        17 => pkt[ihl + 2..ihl + 4].copy_from_slice(&new_port.to_be_bytes()),
        _ => {}
    }
    fix_ip_checksum(pkt);
    fix_l4_checksum(pkt);
}

/// 重写源地址（NAT 反向：src → (orig_dst_ip, orig_dst_port)）。
pub fn rewrite_src(pkt: &mut [u8], new_ip: Ipv4Addr, new_port: u16) {
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    pkt[12..16].copy_from_slice(&new_ip.octets());
    match pkt[9] {
        6 => pkt[ihl..ihl + 2].copy_from_slice(&new_port.to_be_bytes()),
        17 => pkt[ihl..ihl + 2].copy_from_slice(&new_port.to_be_bytes()),
        _ => {}
    }
    fix_ip_checksum(pkt);
    fix_l4_checksum(pkt);
}

/// 构造 TCP RST（源 = orig dst——「目标拒绝」语义；对齐 gVisor Complete(true)/
/// HandleUnknownDestinationPacket 的 RST）。seq 取输入包的 ack（有 ACK）或
/// seq+payload 长（无 ACK），标准 RST 构造。
pub fn build_tcp_rst(input: &Ipv4View<'_>) -> Vec<u8> {
    // 形态对齐 smoltcp `rst_reply`（SYN-SENT 侧只认 RST|ACK 且 ack = iss+1）：
    // seq = 输入的 ack（无则 0）；对 SYN 输入补 ack = seq + SEG.LEN（载荷 + SYN/FIN 位）。
    let syn_fin = (input.tcp_flags & TCP_SYN != 0) as u32 + (input.tcp_flags & TCP_FIN != 0) as u32;
    let seq = if input.tcp_flags & TCP_ACK != 0 {
        input.tcp_ack
    } else {
        0
    };
    let ack = if input.tcp_flags & TCP_SYN != 0 {
        input.tcp_seq.wrapping_add(input.payload.len() as u32 + syn_fin)
    } else {
        input.tcp_ack
    };
    let mut pkt = Vec::with_capacity(40);
    // IPv4 头（20B）：ver/ihl、tos、total 占位、id/flags、ttl、proto=6、校验和占位
    pkt.extend_from_slice(&[0x45, 0]);
    pkt.extend_from_slice(&40u16.to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 0, 0, 64, 6, 0, 0]);
    pkt.extend_from_slice(&input.dst.octets());
    pkt.extend_from_slice(&input.src.octets());
    debug_assert_eq!(pkt.len(), 20);
    // TCP 头（20B）：sport=原 dst_port，dport=原 src_port
    pkt.extend_from_slice(&input.dst_port.to_be_bytes());
    pkt.extend_from_slice(&input.src_port.to_be_bytes());
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&ack.to_be_bytes());
    pkt.extend_from_slice(&[(5 << 4), TCP_RST | TCP_ACK]);
    pkt.extend_from_slice(&0u16.to_be_bytes()); // window
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum 占位
    pkt.extend_from_slice(&0u16.to_be_bytes()); // urgent
    debug_assert_eq!(pkt.len(), 40);
    fix_ip_checksum(&mut pkt);
    fix_l4_checksum(&mut pkt);
    pkt
}

/// 构造 ICMP 错误报文的**内核**（type/code 参数化——Q-K F4）。
///
/// - **无 proto 门**（对齐 gVisor：重组超时对任意 proto 都发，`ipv4/icmp.go:816-830`）；
///   调用方按需自行加门（薄封装 `build_icmp_unreachable` 保留 `proto == 17`）。
/// - **抑制集**（对齐 gVisor `icmp.go:645-647`）：源 `0.0.0.0`、目的组播
///   （224.0.0.0/4）、目的 255.255.255.255 ⇒ 不发（`None`）。
/// - 载荷 = 原 IP 头 + 前 8 字节（RFC 792 最小形态；gVisor 按 RFC 1812 发 ≤548B
///   ——**有意分歧**，登记见 `docs/INTEROP-CRITERIA.md`）。
/// - `orig` 可以是「原包」或「IP 头 + 前 8 字节」前缀（重组器只留前缀——本函数
///   **不**校验 `total_len` 与 `orig.len()` 的一致性；ICMP 载荷按收到的字节原样嵌入）。
pub fn build_icmp(orig: &[u8], ty: u8, code: u8) -> Option<Vec<u8>> {
    if orig.len() < 20 || orig[0] >> 4 != 4 {
        return None;
    }
    let ihl = (orig[0] & 0x0f) as usize * 4;
    if !(20..=60).contains(&ihl) || orig.len() < ihl {
        return None;
    }
    let src = Ipv4Addr::new(orig[12], orig[13], orig[14], orig[15]);
    let dst = Ipv4Addr::new(orig[16], orig[17], orig[18], orig[19]);
    if src == Ipv4Addr::UNSPECIFIED || dst.is_multicast() || dst == Ipv4Addr::BROADCAST {
        return None; // 抑制集（对齐 gVisor）
    }
    let head_len = ihl + 8.min(orig.len() - ihl);
    let orig_head = &orig[..head_len];
    let mut pkt = Vec::with_capacity(20 + 8 + orig_head.len());
    pkt.extend_from_slice(&[0x45, 0, 0, 0]);
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 64, 1, 0, 0]);
    pkt.extend_from_slice(&dst.octets());
    pkt.extend_from_slice(&src.octets());
    pkt.extend_from_slice(&[ty, code, 0, 0]); // type/code + 校验和占位
    pkt.extend_from_slice(&[0, 0, 0, 0]); // unused
    pkt.extend_from_slice(orig_head);
    let total = pkt.len() as u16;
    pkt[2..4].copy_from_slice(&total.to_be_bytes());
    fix_ip_checksum(&mut pkt);
    // ICMP 校验和（伪头部不参与；段在 IP 头之后）
    let icmp_sum_pos = 20 + 2;
    pkt[icmp_sum_pos] = 0;
    pkt[icmp_sum_pos + 1] = 0;
    let sum = checksum(&pkt[20..], 0);
    pkt[icmp_sum_pos..icmp_sum_pos + 2].copy_from_slice(&sum.to_be_bytes());
    Some(pkt)
}

/// 构造 ICMP type3 code3（port unreachable；UDP 会话满/无端点）。
/// 载荷 = 原 IP 头 + 前 8 字节（RFC 792）。**薄封装**：自留 `proto == 17` 门
/// （ICMP 内核本身无门——Q-K F4；本函数语义与 Q-B 起逐字节不变）。
pub fn build_icmp_unreachable(orig: &[u8]) -> Option<Vec<u8>> {
    let f = Ipv4FragHdr::parse(orig)?;
    if f.hdr.proto != 17 {
        return None; // 只对 UDP 回（对齐 gVisor 的 UDP 无监听行为）
    }
    build_icmp(orig, 3, 3)
}

/// 构造 ICMP type11 code1（Time Exceeded / Fragment Reassembly Time Exceeded，Q-K F4）。
/// `orig` = 首片前缀（IP 头 + 前 8 字节）；**任意 proto**（内核无门），抑制集同上。
pub fn build_icmp_reassembly_timeout(orig: &[u8]) -> Option<Vec<u8>> {
    build_icmp(orig, 11, 1)
}

/// RFC 1624 的**增量校验和更新**（纯函数面）：把校验和从 `hc` 更新为「`pairs` 里每个
/// 16 位字从旧值改成新值」后的值。`HC' = ~(~HC + Σ(~m + m'))`（一补数加法）。
///
/// **唯一用途 = 分片首片的 L4 校验和**（Q-K F5-d）：整报文载荷不在本片内，拿不到
/// 全量重算的输入；改动字 = 伪头源地址 2 个 16 位字 + L4 源端口 1 个 16 位字。
/// 结果 `0x0000` 由调用方按 UDP 规则写 `0xFFFF`（RFC 768）；**原值 0（无校验）不得
/// 被本函数变成非 0**（调用方前置判断）。
pub fn rfc1624_update(hc: u16, pairs: &[(u16, u16)]) -> u16 {
    let mut sum: u32 = !hc as u32;
    for &(old, new) in pairs {
        sum += !old as u32;
        sum += new as u32;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// 分片**首片**的反向重写（Q-K F5-d）：改 IP 源地址 + UDP 源端口，L4 校验和用
/// RFC 1624 增量更新（**不能**全量重算——整报文载荷不在本片内）。
///
/// 前置：调用方已判 `proto == 17` 且 `Ipv4FragHdr::parse` 成功。UDP 校验和原值
/// 0（RFC 768「无校验」）⇒ **保持 0**（不得被 NAT 变成非 0）。
pub fn rewrite_src_first_fragment(pkt: &mut [u8], new_ip: Ipv4Addr, new_port: u16) {
    if pkt.len() < 20 {
        return;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    if ihl < 20 || pkt.len() < ihl + 8 {
        return;
    }
    let old_ip = [pkt[12], pkt[13], pkt[14], pkt[15]];
    let new_ip_b = new_ip.octets();
    let old_port = u16::from_be_bytes([pkt[ihl], pkt[ihl + 1]]);
    let ck = u16::from_be_bytes([pkt[ihl + 6], pkt[ihl + 7]]);
    if ck != 0 {
        let pairs = [
            (u16::from_be_bytes([old_ip[0], old_ip[1]]), u16::from_be_bytes([new_ip_b[0], new_ip_b[1]])),
            (u16::from_be_bytes([old_ip[2], old_ip[3]]), u16::from_be_bytes([new_ip_b[2], new_ip_b[3]])),
            (old_port, new_port),
        ];
        let c = rfc1624_update(ck, &pairs);
        // UDP：算得 0 写 0xFFFF（0x0000 = 「无校验」语义）
        let c = if c == 0 { 0xFFFF } else { c };
        pkt[ihl + 6..ihl + 8].copy_from_slice(&c.to_be_bytes());
    }
    pkt[12..16].copy_from_slice(&new_ip_b);
    pkt[ihl..ihl + 2].copy_from_slice(&new_port.to_be_bytes());
    fix_ip_checksum(pkt);
}

/// 构造最小 TCP SYN 假包（测试面：驱动 RX 路径的输入形态——**带 MSS option**，
/// smoltcp 服务端要求 SYN 携带 MSS 才接受；形态对齐真栈产 SYN：IP20 + TCP24）。
pub fn build_tcp_syn(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16, seq: u32) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(44);
    pkt.extend_from_slice(&[0x45, 0]);
    pkt.extend_from_slice(&44u16.to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 0, 0, 64, 6, 0, 0]);
    pkt.extend_from_slice(&src.octets());
    pkt.extend_from_slice(&dst.octets());
    debug_assert_eq!(pkt.len(), 20);
    pkt.extend_from_slice(&sport.to_be_bytes());
    pkt.extend_from_slice(&dport.to_be_bytes());
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&0u32.to_be_bytes());
    pkt.extend_from_slice(&[(6 << 4), TCP_SYN]); // data offset 6（24B 头）
    pkt.extend_from_slice(&0xFFFFu16.to_be_bytes()); // window
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum 占位
    pkt.extend_from_slice(&0u16.to_be_bytes()); // urgent
    pkt.extend_from_slice(&[0x02, 0x04, 0x04, 0xD8]); // MSS = 1240（MTU1280-40，对齐真栈）
    debug_assert_eq!(pkt.len(), 44);
    fix_ip_checksum(&mut pkt);
    fix_l4_checksum(&mut pkt);
    pkt
}

/// 构造最小 UDP 包（测试面）。
pub fn build_udp(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(28 + payload.len());
    pkt.extend_from_slice(&[0x45, 0]);
    pkt.extend_from_slice(&((28 + payload.len()) as u16).to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 0, 0, 64, 17, 0, 0]);
    pkt.extend_from_slice(&src.octets());
    pkt.extend_from_slice(&dst.octets());
    debug_assert_eq!(pkt.len(), 20);
    pkt.extend_from_slice(&sport.to_be_bytes());
    pkt.extend_from_slice(&dport.to_be_bytes());
    pkt.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    pkt.extend_from_slice(&0u16.to_be_bytes()); // checksum 占位
    pkt.extend_from_slice(payload);
    debug_assert_eq!(pkt.len(), 28 + payload.len());
    fix_ip_checksum(&mut pkt);
    fix_l4_checksum(&mut pkt);
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析/重写/校验和往返：重写后校验和自洽（重算 == 0 校验通过）。
    #[test]
    fn rewrite_keeps_checksums_valid() {
        let mut pkt = build_tcp_syn(
            Ipv4Addr::new(100, 64, 10, 1),
            40000,
            Ipv4Addr::new(1, 2, 3, 4),
            443,
            100,
        );
        let v = Ipv4View::parse(&pkt).unwrap();
        assert!(v.is_tcp_syn());
        assert_eq!(v.five_tuple(), (Ipv4Addr::new(100, 64, 10, 1), 40000, Ipv4Addr::new(1, 2, 3, 4), 443));
        // 正向重写
        rewrite_dst(&mut pkt, Ipv4Addr::new(100, 64, 255, 1), 20001);
        let v2 = Ipv4View::parse(&pkt).unwrap();
        assert_eq!(v2.dst, Ipv4Addr::new(100, 64, 255, 1));
        assert_eq!(v2.dst_port, 20001);
        assert_eq!(v2.src_port, 40000);
        // 反向重写的对象是**应答包**（src=栈:rw_port）——src 还原为原始目的
        let mut resp = build_tcp_syn(
            Ipv4Addr::new(100, 64, 255, 1),
            20001,
            Ipv4Addr::new(100, 64, 10, 1),
            40000,
            1,
        );
        rewrite_src(&mut resp, Ipv4Addr::new(1, 2, 3, 4), 443);
        let v3 = Ipv4View::parse(&resp).unwrap();
        assert_eq!(
            v3.five_tuple(),
            (Ipv4Addr::new(1, 2, 3, 4), 443, Ipv4Addr::new(100, 64, 10, 1), 40000),
            "应答的源应还原为原始目的（客户端看到的 src）"
        );
        // IP 头校验和验证（对头部求和应为 0xFFFF 补码后为 0）
        let ihl = 20;
        let sum = checksum(&pkt[..ihl], 0);
        assert_eq!(sum, 0, "IP 校验和应自洽");
        // TCP 校验和验证（伪头部 + 段求和为 0）
        let pseudo = [
            pkt[12], pkt[13], pkt[14], pkt[15], pkt[16], pkt[17], pkt[18], pkt[19], 0, 6,
            ((pkt.len() - ihl) >> 8) as u8,
            (pkt.len() - ihl) as u8,
        ];
        assert_eq!(
            checksum_raw(&pseudo, checksum_raw(&pkt[20..], 0)),
            0xFFFF,
            "TCP 校验和应自洽（标准形）"
        );
    }

    /// UDP 重写 + 校验和（含 0xFFFF 规范形态的健壮性——非零路径）。
    #[test]
    fn udp_rewrite_and_checksum() {
        let mut pkt = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            53,
            b"query-payload",
        );
        rewrite_dst(&mut pkt, Ipv4Addr::new(100, 64, 255, 1), 20002);
        let v = Ipv4View::parse(&pkt).unwrap();
        assert_eq!(v.dst_port, 20002);
        assert_eq!(v.payload, b"query-payload");
        let pseudo = [
            pkt[12], pkt[13], pkt[14], pkt[15], pkt[16], pkt[17], pkt[18], pkt[19], 0, 17,
            ((pkt.len() - 20) >> 8) as u8,
            (pkt.len() - 20) as u8,
        ];
        assert_eq!(
            checksum_raw(&pseudo, checksum_raw(&pkt[20..], 0)),
            0xFFFF,
            "UDP 校验和应自洽（标准形）"
        );
    }

    /// RST 构造：源 = 原目的；校验和自洽；可直接进 encap 出站。
    #[test]
    fn rst_shape() {
        let input = build_tcp_syn(
            Ipv4Addr::new(100, 64, 10, 1),
            40000,
            Ipv4Addr::new(1, 2, 3, 4),
            443,
            100,
        );
        let v = Ipv4View::parse(&input).unwrap();
        let rst = build_tcp_rst(&v);
        let rv = Ipv4View::parse(&rst).unwrap();
        assert_eq!(rv.src, Ipv4Addr::new(1, 2, 3, 4), "RST 源 = 原目的");
        assert_eq!(rv.src_port, 443);
        assert_eq!(rv.dst_port, 40000);
        assert_eq!(rv.tcp_flags & TCP_RST, TCP_RST);
        // 对 SYN 输入：ack = seq + SEG.LEN（无载荷 SYN ⇒ +1）；seq = 输入 ack（无 ⇒ 0）
        assert_eq!(rv.tcp_ack, 101);
        assert_eq!(rv.tcp_seq, 0);
        let pseudo = [
            rst[12], rst[13], rst[14], rst[15], rst[16], rst[17], rst[18], rst[19], 0, 6,
            ((rst.len() - 20) >> 8) as u8,
            (rst.len() - 20) as u8,
        ];
        assert_eq!(checksum_raw(&pseudo, checksum_raw(&rst[20..], 0)), 0xFFFF);
    }

    /// ICMP 构造：type3/code3、载荷 = 原 IP 头 + 8B、校验和自洽。
    #[test]
    fn icmp_unreachable_shape() {
        let orig = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            53,
            b"dns-q",
        );
        let icmp = build_icmp_unreachable(&orig).unwrap();
        assert_eq!(icmp[20], 3);
        assert_eq!(icmp[21], 3);
        assert_eq!(&icmp[28..28 + 8], &orig[..8], "载荷含原 IP 头前 8B");
        // ICMP 校验和
        assert_eq!(checksum_raw(&icmp[20..], 0), 0xFFFF);
        // TCP 输入不产 ICMP（对齐 gVisor——TCP 无监听回 RST 不是 ICMP）
        let syn = build_tcp_syn(Ipv4Addr::new(1, 1, 1, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, 1);
        assert!(build_icmp_unreachable(&syn).is_none());
    }

    /// F7：分片字段判定——非首片（frag_off>0）/ MF 置位 = 分片；DF-only 不误判。
    #[test]
    fn fragment_flag_detection() {
        let mk = || {
            build_udp(
                Ipv4Addr::new(100, 64, 10, 1),
                50000,
                Ipv4Addr::new(1, 2, 3, 4),
                53,
                b"x",
            )
        };
        let mut p = mk();
        assert!(!Ipv4View::parse(&p).unwrap().is_fragment(), "普通包非分片");
        assert!(!Ipv4FragHdr::parse(&p).unwrap().hdr.is_fragment());
        p[6] = 0x20; // MF（More Fragments）
        assert!(Ipv4View::parse(&p).unwrap().is_fragment(), "MF 置位 = 分片");
        assert!(Ipv4FragHdr::parse(&p).unwrap().hdr.is_fragment());
        p[6] = 0x00;
        p[7] = 0x08; // frag_off = 8（非首片）
        assert!(Ipv4View::parse(&p).unwrap().is_fragment(), "非首片 = 分片");
        assert!(Ipv4FragHdr::parse(&p).unwrap().hdr.is_fragment());
        p[6] = 0x40; // DF-only
        p[7] = 0x00;
        assert!(!Ipv4View::parse(&p).unwrap().is_fragment(), "DF-only 不算分片");
        assert!(!Ipv4FragHdr::parse(&p).unwrap().hdr.is_fragment(), "DF-only（轻量视图同判）");
        p[6] = 0x60; // DF | MF
        assert!(Ipv4View::parse(&p).unwrap().is_fragment(), "DF+MF 仍是分片");
        p[6] = 0x40;
        p[7] = 0x08; // DF + frag_off
        assert!(Ipv4View::parse(&p).unwrap().is_fragment(), "DF+偏移仍是分片");
    }

    /// Q-K T11：轻量 IP 头视图的畸形输入 ⇒ None（静默丢，不进重组器）。
    #[test]
    fn frag_header_parse_malformed_inputs() {
        let base = build_udp(Ipv4Addr::new(1, 2, 3, 4), 1, Ipv4Addr::new(5, 6, 7, 8), 9, b"abc");
        assert!(Ipv4FragHdr::parse(&base).is_some());
        // 截断（< 20）
        assert!(Ipv4FragHdr::parse(&base[..19]).is_none());
        // version != 4
        let mut p = base.clone();
        p[0] = 0x65;
        assert!(Ipv4FragHdr::parse(&p).is_none());
        // ihl < 20
        let mut p = base.clone();
        p[0] = 0x44;
        assert!(Ipv4FragHdr::parse(&p).is_none());
        // total_len < ihl
        let mut p = base.clone();
        p[2..4].copy_from_slice(&12u16.to_be_bytes());
        assert!(Ipv4FragHdr::parse(&p).is_none());
        // total_len > pkt.len()
        let mut p = base.clone();
        p[2..4].copy_from_slice(&999u16.to_be_bytes());
        assert!(Ipv4FragHdr::parse(&p).is_none());
        // 合法：字段取值（ident/off/mf/proto/头长）
        let mut p = base.clone();
        p[4..6].copy_from_slice(&0x1234u16.to_be_bytes());
        p[6..8].copy_from_slice(&(0x2000u16 | 5).to_be_bytes());
        let f = Ipv4FragHdr::parse(&p).unwrap();
        assert_eq!(f.hdr.ident, 0x1234);
        assert_eq!(f.hdr.frag_off, 5);
        assert!(f.hdr.mf);
        assert_eq!(f.hdr.proto, 17);
        assert_eq!(f.hdr.header_len, 20);
        assert_eq!(f.head.len(), 20);
        assert_eq!(f.head, &p[..20]);
    }

    /// Q-K T12：片载荷取段钉死 = `pkt[ihl..total_len]`——尾随字节不进片载荷。
    #[test]
    fn frag_slice_payload_excludes_trailing_bytes() {
        let mut p = build_udp(Ipv4Addr::new(1, 2, 3, 4), 1, Ipv4Addr::new(5, 6, 7, 8), 9, b"abc");
        p.extend_from_slice(&[0xEE; 8]); // 尾随填充（total_len 不含）
        let f = Ipv4FragHdr::parse(&p).unwrap();
        assert_eq!(f.payload, &p[20..31], "片载荷 = pkt[ihl..total_len]");
        assert!(!f.payload.contains(&0xEE));
    }

    /// Q-K F5-d：RFC 1624 增量更新 == 全量重算（独立路径对照——`rewrite_src` 走
    /// `fix_l4_checksum` 全量）。含头部校验和与环境字段的逐字节对照。
    #[test]
    fn rfc1624_matches_full_recompute() {
        let payload = vec![0x5Au8; 1400];
        let mut full = build_udp(
            Ipv4Addr::new(100, 64, 255, 1),
            20001,
            Ipv4Addr::new(100, 64, 10, 5),
            45000,
            &payload,
        );
        let mut incr = full.clone();
        rewrite_src(&mut full, Ipv4Addr::new(93, 184, 216, 34), 443);
        rewrite_src_first_fragment(&mut incr, Ipv4Addr::new(93, 184, 216, 34), 443);
        assert_eq!(full[12..16], incr[12..16], "IP 源一致");
        assert_eq!(full[20..26], incr[20..26], "UDP 头前 6 字节（含源端口）一致");
        assert_eq!(full[26..28], incr[26..28], "UDP 校验和：增量 == 全量");
        assert_eq!(checksum_raw(&incr[..20], 0), 0xFFFF, "IP 校验和自洽");
        assert_eq!(&full[28..], &incr[28..], "载荷不动");
        // UDP 校验和原值 0（RFC 768 无校验）⇒ 保持 0，不得被 NAT 变成非 0
        let mut z = full.clone();
        z[26] = 0;
        z[27] = 0;
        rewrite_src_first_fragment(&mut z, Ipv4Addr::new(1, 1, 1, 1), 1234);
        assert_eq!(&z[26..28], &[0, 0], "原值 0 保持 0");
    }

    /// Q-K F4：ICMP 内核——type/code 参数化、无 proto 门、抑制集、载荷 = 头 + 8B。
    #[test]
    fn icmp_core_type_code_and_suppression() {
        let orig = build_udp(
            Ipv4Addr::new(100, 64, 10, 1),
            50000,
            Ipv4Addr::new(8, 8, 8, 8),
            53,
            b"dns-q-abcdef",
        );
        // type 11 code 1（超时）
        let icmp = build_icmp(&orig, 11, 1).unwrap();
        assert_eq!((icmp[20], icmp[21]), (11, 1));
        assert_eq!(&icmp[12..16], &[8, 8, 8, 8], "src = 原目的");
        assert_eq!(&icmp[16..20], &[100, 64, 10, 1], "dst = 原源");
        assert_eq!(&icmp[28..48], &orig[..20], "载荷含原 IP 头");
        assert_eq!(&icmp[48..56], &orig[20..28], "载荷含前 8 字节");
        assert_eq!(checksum_raw(&icmp[..20], 0), 0xFFFF, "IP 校验和自洽");
        assert_eq!(checksum_raw(&icmp[20..], 0), 0xFFFF, "ICMP 校验和自洽");
        // 内核无 proto 门：TCP 输入也能发 11/1（gVisor 同形）
        let syn = build_tcp_syn(Ipv4Addr::new(1, 1, 1, 1), 1, Ipv4Addr::new(2, 2, 2, 2), 2, 1);
        assert!(build_icmp(&syn, 11, 1).is_some(), "TCP 也发（内核无门）");
        assert!(build_icmp_reassembly_timeout(&syn).is_some());
        // 薄封装保留 UDP 门（type3/code3）
        assert!(build_icmp_unreachable(&syn).is_none(), "port unreachable 仍只对 UDP");
        // 抑制集：源 0.0.0.0 / 目的组播 / 目的 255.255.255.255
        let mut p = orig.clone();
        p[12..16].copy_from_slice(&[0, 0, 0, 0]);
        assert!(build_icmp(&p, 11, 1).is_none(), "源 0.0.0.0 抑制");
        let mut p = orig.clone();
        p[16..20].copy_from_slice(&[224, 0, 0, 1]);
        assert!(build_icmp(&p, 11, 1).is_none(), "目的组播抑制");
        let mut p = orig.clone();
        p[16..20].copy_from_slice(&[255, 255, 255, 255]);
        assert!(build_icmp(&p, 11, 1).is_none(), "目的广播抑制");
        // 「首片前缀」形态（total_len 与实际长度不一致）也要能构 ICMP：
        // 重组器只留 IP 头 + 8B，头里的 total_len 仍是原分片值。
        let mut frag = orig[..28].to_vec();
        frag[2..4].copy_from_slice(&1276u16.to_be_bytes()); // 声明长度 > 实际（前缀形态）
        assert!(build_icmp_reassembly_timeout(&frag).is_some(), "前缀形态可构");
        assert_eq!(build_icmp_reassembly_timeout(&frag).unwrap().len(), 20 + 8 + 28);
    }
}
