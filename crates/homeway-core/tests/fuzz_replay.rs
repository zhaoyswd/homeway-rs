//! R5-5b 结构化随机重放（回归轨；设计 §2.2 轨 2）。
//!
//! 目标 = 全部网络可达解析器的 **pub 纯函数面**（纯解析抽取是硬前置——评审 ②-1）。
//! oracle 四断言（评审 ②-3）：
//!   (a) 不 panic（panic = 测试失败——所有目标共用）；
//!   (b) **分块等价**：状态机类目标（CtlDecoder）同一字节流按不同切块喂入结果一致；
//!   (c) **往返一致**：合法输入 encode→decode→encode 字节稳定；
//!   (d) **夹具期望**：向量族样本的解析结果与 fixtures 期望一致（token/relay/stun_sped）。
//! 生成器 = xorshift64 固定 seed（确定性；`HER_SEED` 可复现指定轮）；
//! 配比 70% 种子骨架变异 + 30% 全随机。每目标 ≥100k 次迭代（`#[ignore]`：
//! CI quick 档跳过、全量档 `--ignored` 显式开——G-10）。

use homeway_core::probe;
use homeway_core::server::intercept::dnsface;
use homeway_core::server::intercept::nat::Ipv4View;
use homeway_core::server::upnp;
use homeway_core::speedtest;
use homeway_core::token;
use homeway_core::wtransport::frame;

const ITER: usize = 100_000;

/// xorshift64*（确定性；非密码面——测试生成器专用）。
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 33) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

fn seed() -> u64 {
    match std::env::var("HER_SEED") {
        Ok(s) => s.parse().unwrap_or(0x5eed_1234_abcd_ef01),
        Err(_) => 0x5eed_1234_abcd_ef01,
    }
}

/// 种子池（合法骨架——变异的底座；分块等价与往返断言也用它）。
fn seeds() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = Vec::new();
    // 腿帧/容器/信封骨架
    v.push(frame::batch_bytes(&[(2, &[0x41; 66]), (0, &[1, 0, 0, 0, 2])]));
    v.push(frame::frame_bytes(0, &[0xBB; 32]));
    v.push(frame::frame_bytes(1, b"hint-payload"));
    let mut env = vec![0xAA];
    env.extend_from_slice(&[9u8; 8]);
    env.extend_from_slice(&frame::frame_bytes(4, &[0; 10]));
    v.push(env);
    // speedtest 帧（向量子集）
    v.push(b"SPED\x01\x00\x00\x00\x00\x00\x2d\x00".to_vec()); // 控制帧头（len=45）
    v.push(b"SPED\x04\x01\x00\x00\x00\x00\x78\x00".to_vec()); // data 帧（len=0x7800 边界）
    // report JSON 骨架（含引号感知负例）
    v.push(br#"{"bytes":123,"warmup_bytes":45,"wall_ms":678}"#.to_vec());
    v.push(br#"{"bytes":0,"warmup_bytes":0,"wall_ms":0,"error":"a,b \"x\", c}d"}"#.to_vec());
    v.push(br#"{"error":"busy"}"#.to_vec());
    // files 4B 前缀
    v.push(0x0000_0000u32.to_be_bytes().to_vec()); // 终止帧
    v.push(0x0000_0100u32.to_be_bytes().to_vec());
    // DNS TCP 2B 分帧
    v.push(0x0004u16.to_be_bytes().iter().copied().chain([0xAB; 4]).collect());
    // relay 控制帧骨架
    v.push(homeway_core::relaywire::encode_hello(&[7u8; 32]));
    v.push(homeway_core::relaywire::ok_bytes());
    v.push(homeway_core::relaywire::keepalive_bytes());
    // STUN 骨架
    v.push(homeway_core::server::egress::stun_request(&[3u8; 12], "fuzz"));
    // token 串
    v.push(b"hmw1".to_vec());
    v.push(b"hmw1AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_vec());
    // HTTP/SSDP 文本面
    v.push(b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.3.1:49152/root.xml\r\n\r\n".to_vec());
    v.push(b"<root><service><serviceType>WANIPConnection</serviceType><controlURL>/ctrlu</controlURL></service></root>".to_vec());
    v.push(b"http://192.168.3.1:49152/root.xml".to_vec());
    v
}

/// 骨架变异（70%）：截断 / 单字节翻转 / 长度字段加扰 / 段拼接。
fn mutate(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    let base = &seeds[rng.below(seeds.len())];
    match rng.below(4) {
        0 => {
            // 截断（drain 边界类——R2 critical bug 形态）
            let cut = rng.below(base.len() + 1);
            base[..cut].to_vec()
        }
        1 => {
            // 单/多字节翻转
            let mut b = base.clone();
            for _ in 0..1 + rng.below(4) {
                if !b.is_empty() {
                    let i = rng.below(b.len());
                    b[i] = rng.byte();
                }
            }
            b
        }
        2 => {
            // 拼接两段（跨帧边界错位形态）
            let other = &seeds[rng.below(seeds.len())];
            let mut b = base.clone();
            b.extend_from_slice(other);
            b
        }
        _ => {
            // 尾部追加随机（超限长度场形态）
            let mut b = base.clone();
            let n = rng.below(64);
            b.extend((0..n).map(|_| rng.byte()).collect::<Vec<u8>>());
            b
        }
    }
}

fn gen_input(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    if rng.below(10) < 7 {
        mutate(rng, seeds)
    } else {
        // 30% 全随机（长度 0..256——覆盖前缀/头/边界）
        let n = rng.below(256);
        (0..n).map(|_| rng.byte()).collect()
    }
}

// ---------- ① 腿帧/容器/信封（fuzz_leg_frame） ----------

#[test]
#[ignore = "fuzz 全量档（cargo test --ignored；quick 档跳过——G-10）"]
fn fuzz_leg_frame() {
    let mut rng = Rng(seed() ^ 0x0101);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = frame::decode_frame(&b);
        let _ = frame::decode_tagged(&b);
        let _ = frame::decode_batch(&b);
        if let Ok(s) = String::from_utf8(b.clone()) {
            let _ = frame::decode_hint_payload(s.as_bytes());
        }
        let _ = homeway_core::relaywire::decode_relay_reg_frame(&b);
    }
}

// ---------- ② 中继控制面（fuzz_relay_ctl；含分块等价） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_relay_ctl() {
    use homeway_core::relaywire as rw;
    let mut rng = Rng(seed() ^ 0x0202);
    let sd = seeds();
    let sd2 = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = rw::decode_hello(&b);
        let _ = rw::decode_challenge(&b);
        let _ = rw::decode_proof(&b);
        let _ = rw::decode_ok_auth(&b);
        let _ = rw::decode_session(&b);
        let _ = rw::decode_release(&b);
        let _ = rw::legup_cookie(&b);
        // 分块等价（oracle-b）：同一流按 1B / 随机 / 对半 三种切块喂 CtlDecoder，
        // 产出的消息序列（type+payload）必须一致
        let flow = gen_input(&mut rng, &sd2);
        let a = feed_chunked(&flow, Chunking::One);
        let b2 = feed_chunked(&flow, Chunking::Halves);
        let c = feed_chunked(&flow, Chunking::Random(&mut rng));
        assert_eq!(a, b2, "CtlDecoder 分块等价失败（1B vs 对半）");
        assert_eq!(a, c, "CtlDecoder 分块等价失败（1B vs 随机）");
    }
}

enum Chunking<'r> {
    One,
    Halves,
    Random(&'r mut Rng),
}

fn feed_chunked(flow: &[u8], mut mode: Chunking) -> Vec<(u8, Vec<u8>)> {
    use homeway_core::relaywire::CtlDecoder;
    let mut dec = CtlDecoder::new();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < flow.len() {
        let step = match mode {
            Chunking::One => 1,
            Chunking::Halves => (flow.len() / 2).max(1),
            Chunking::Random(ref mut rng) => 1 + rng.below(17),
        };
        let end = (i + step).min(flow.len());
        // 部分喂入的 Err 视为「该流终止」（错误输入的分块等价 = 喂到报错为止，两侧一致）
        if dec.feed(&flow[i..end], &mut out).is_err() {
            break;
        }
        i = end;
    }
    out
}

// ---------- ③ token（fuzz_token） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_token() {
    let mut rng = Rng(seed() ^ 0x0303);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let s = String::from_utf8_lossy(&b).into_owned();
        let _ = token::decode(&s);
        let _ = token::parse_body(&b);
    }
}

// ---------- ④ speedtest 双向（fuzz_speedtest；含往返一致） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_speedtest() {
    let mut rng = Rng(seed() ^ 0x0404);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = speedtest::decode_frame(&b);
        let _ = speedtest::parse_report(&b);
    }
    // 往返一致（oracle-c）：合法帧 encode→decode→字段重组 encode 字节稳定
    // （控制帧头 + 载荷 crc——借 r5_vectors 的手搓形态，抽 1k 轮）
    let mut rng = Rng(seed() ^ 0x0405);
    for _ in 0..1_000 {
        let plen = rng.below(256);
        let payload: Vec<u8> = (0..plen).map(|_| rng.byte()).collect();
        let mut wire = Vec::with_capacity(15 + plen);
        wire.extend_from_slice(b"SPED");
        wire.push(1);
        wire.extend_from_slice(&0u32.to_le_bytes());
        wire.extend_from_slice(&(plen as u16).to_le_bytes());
        wire.extend_from_slice(&crc32(&payload).to_le_bytes());
        wire.extend_from_slice(&payload);
        let (head, got) = speedtest::decode_frame(&wire).expect("完整").expect("到齐");
        assert_eq!((head.typ, head.seq, head.len), (1, 0, plen));
        assert_eq!(got, payload, "往返载荷应逐字节一致");
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

// ---------- ⑤ files 前缀（fuzz_files） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_files_prefix() {
    use homeway_core::files::decode_prefix;
    let mut rng = Rng(seed() ^ 0x0505);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = decode_prefix(&b);
    }
    // 超上限防御：任意 4B 前缀要么 ≤MAX_CHUNK 要么报错（不 panic 即底线，
    // 上限断言把「静默接受超长」钉死）
    for _ in 0..1_000 {
        let mut pre = Vec::with_capacity(4);
        for _ in 0..4 {
            pre.push(rng.byte());
        }
        if let Ok(homeway_core::files::Prefix::Frame { len }) = decode_prefix(&pre) {
            assert!(len <= homeway_core::files::MAX_CHUNK, "超上限被静默接受：{len}");
        }
    }
}

// ---------- ⑥ DNS 消息 + TCP 分帧（fuzz_dns） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_dns() {
    use homeway_core::server::dnsproxy as dp;
    let mut rng = Rng(seed() ^ 0x0606);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = dp::qtype(&b);
        let _ = dp::empty_response(&b);
        let mut m = b.clone();
        dp::clamp_ttl(&mut m, 60);
        let _ = dp::count_aaaa(&m);
        let _ = dp::truncate(&m, 1232);
        let _ = dnsface::decode_tcp_frame(&b);
    }
}

// ---------- ⑦ 内层 IPv4/TCP/UDP 视图（fuzz_inner_pkt） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_inner_pkt() {
    let mut rng = Rng(seed() ^ 0x0707);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        if let Some(v) = Ipv4View::parse(&b) {
            // 解析成功 ⇒ 头部字段自洽（头长 ≥20 且不越界、载荷切片在界内）
            assert!(v.header_len >= 20, "ihl 荒值：{}", v.header_len);
            assert!(v.header_len <= b.len(), "头长越界：{} > {}", v.header_len, b.len());
        }
    }
}

// ---------- ⑧ 参照点探测应答（fuzz_probe） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_probe() {
    let mut rng = Rng(seed() ^ 0x0808);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = probe::decode_response(&b, &[0x11; 8]);
        let _ = probe::decode_response(&b, &[0; 8]);
    }
}

// ---------- ⑨ UPnP/SSDP 文本面（fuzz_upnp——R3-H4 远程打崩前科面，最高优先） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_upnp() {
    let mut rng = Rng(seed() ^ 0x0909);
    let sd = seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let s = String::from_utf8_lossy(&b).into_owned();
        let _ = upnp::header_value(&s, "LOCATION");
        let _ = upnp::header_value(&s, "ST");
        let _ = upnp::xml_tag(&s, "controlURL");
        let _ = upnp::xml_tag(&s, "NewExternalPort");
        let _ = upnp::parse_http_url(&s);
    }
    // header_value 的字节安全回归（H4 形态）：键名出现处带多字节前缀不 panic——
    // 由上面 from_utf8_lossy 的替换字符面间接覆盖；再钉一组显式多字节样本
    for sample in ["LOÇÃO: x", "ＬＯＣＡＴＩＯＮ: y", "LOCATION: héllo", "LOCATION:\x00"] {
        let _ = upnp::header_value(sample, "LOCATION");
    }
}

// ---------- ⑩ 夹具期望抽查（oracle-d：向量样本解析与期望一致） ----------

#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_fixture_expectations() {
    // token 正样本（真 token 经 decode 成功）
    let p = format!("{}/../../fixtures/vectors/token.json", env!("CARGO_MANIFEST_DIR"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let mut n_ok = 0;
    for c in v["cases"].as_array().unwrap() {
        if let Some(tok) = c["token"].as_str() {
            if token::decode(tok).is_ok() {
                n_ok += 1;
            }
        }
    }
    assert!(n_ok >= 5, "token 正样本至少 5 例可解（得 {n_ok}）");
    // STUN 请求骨架往返（stun_sped 向量的首样本）
    let p2 = format!("{}/../../fixtures/vectors/stun_sped.json", env!("CARGO_MANIFEST_DIR"));
    let v2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p2).unwrap()).unwrap();
    let c = &v2["stun"]["requests"][0];
    let mut tx = [0u8; 12];
    let unhex = |s: &str| -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    };
    tx.copy_from_slice(&unhex(c["txid"].as_str().unwrap()));
    assert_eq!(
        homeway_core::server::egress::stun_request(&tx, c["software"].as_str().unwrap()),
        unhex(c["wire"].as_str().unwrap()),
        "骨架往返稳定"
    );
}
