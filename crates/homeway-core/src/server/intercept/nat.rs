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
        })
    }

    /// 过境五元组键（UDP 会话表与 TCP 映射共用形态）。
    pub fn five_tuple(&self) -> (Ipv4Addr, u16, Ipv4Addr, u16) {
        (self.src, self.src_port, self.dst, self.dst_port)
    }

    pub fn is_tcp_syn(&self) -> bool {
        self.proto == 6 && self.tcp_flags & TCP_SYN != 0 && self.tcp_flags & TCP_ACK == 0
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
fn fix_ip_checksum(pkt: &mut [u8]) {
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

/// 构造 ICMP type3 code3（port unreachable；UDP 会话满/无端点）。
/// 载荷 = 原 IP 头 + 前 8 字节（RFC 792）。
pub fn build_icmp_unreachable(orig: &[u8]) -> Option<Vec<u8>> {
    let view = Ipv4View::parse(orig)?;
    if view.proto != 17 {
        return None; // 只对 UDP 回（对齐 gVisor 的 UDP 无监听行为）
    }
    let orig_head = &orig[..view.header_len + 8.min(orig.len() - view.header_len)];
    let mut pkt = Vec::with_capacity(20 + 8 + orig_head.len());
    pkt.extend_from_slice(&[0x45, 0, 0, 0]);
    pkt.extend_from_slice(&0u16.to_be_bytes());
    pkt.extend_from_slice(&[0, 0, 64, 1, 0, 0]);
    pkt.extend_from_slice(&view.dst.octets());
    pkt.extend_from_slice(&view.src.octets());
    pkt.extend_from_slice(&[3, 3, 0, 0]); // type=3 code=3 + 校验和占位
    pkt.extend_from_slice(&[0, 0, 0, 0]); // unused
    pkt.extend_from_slice(orig_head);
    let total = pkt.len() as u16;
    pkt[2..4].copy_from_slice(&total.to_be_bytes());
    fix_ip_checksum(&mut pkt);
    // ICMP 校验和（类型 3：伪头部不参与；位在 IP 头后的 type/code/sum 段）
    let icmp_sum_pos = 20 + 2;
    pkt[icmp_sum_pos] = 0;
    pkt[icmp_sum_pos + 1] = 0;
    let sum = checksum(&pkt[20..], 0);
    pkt[icmp_sum_pos..icmp_sum_pos + 2].copy_from_slice(&sum.to_be_bytes());
    Some(pkt)
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
}
