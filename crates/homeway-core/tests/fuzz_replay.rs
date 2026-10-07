//! R5-5b 结构化随机重放（回归轨；设计 §2.2 轨 2）。
//!
//! 目标 = 全部网络可达解析器的 **pub 纯函数面**（纯解析抽取是硬前置——评审 ②-1；
//! 服务端半边 speedtest_server::parse_request 同入面——第二道门 高-4 整改）。
//! oracle 形态（②-3 + 第二道门中-14/中-15 整改）：
//!   (a) 不 panic（panic = 测试失败——所有目标共用）；
//!   (b) **分块等价**：状态机类目标（CtlDecoder）同一字节流按不同切块喂入结果一致
//!       （种子含两消息首尾相接 + 尾部半条的流骨架——drain 边界形态钉住）；
//!   (c) **往返一致**：合法输入手搓 wire → decode → 字段/载荷逐比对（编码器私有，
//!       非真 encode→decode→encode——见 r5_vectors.rs 同源副本注记）；
//!   (d) **夹具期望**：token 逐字段（peer_id/secret/端点三元组）+ 负例哨兵码 +
//!       STUN 首样本字节；
//!   (e) **输出上界**：empty_response/truncate/TCP 分帧/header_value/xml_tag/
//!       parse_http_url 等「输出 ≤ 已知上界」断言（独立于实现内 cap；probe 端点数
//!       与 build 长度两条与解析器自身守卫同谓词——回归哨兵，不算独立上界）。
//! 生成器 = xorshift64 固定 seed（确定性；`HER_SEED` 可复现指定轮，非法值即 panic）；
//! 配比 70% 种子骨架变异 + 30% 全随机。每目标 ≥100k 次迭代（`#[ignore]`：
//! CI quick 档跳过、全量档 `--ignored` 显式开——G-10；量级门见 fuzz_iteration_budget）。

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
        Ok(s) => s.parse().unwrap_or_else(|_| {
            panic!("HER_SEED 非法（{s:?}）——须为 u64 十进制；缺省不设即用固定 seed")
        }),
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
    // CtlDecoder 流骨架：两消息首尾相接 + 尾部半条（drain 边界形态——第二道门 中-10：
    // 随机/变异输入过「长度行 ≤256」闸的概率 ~2e-3，不用骨架钉住则 100k 轮几乎观测不到）
    {
        let m1 = homeway_core::relaywire::ok_bytes();
        let m2 = homeway_core::relaywire::keepalive_bytes();
        let mut flow = Vec::new();
        homeway_core::relaywire::ctl_frame_into(&m1, &mut flow);
        homeway_core::relaywire::ctl_frame_into(&m2, &mut flow);
        flow.extend_from_slice(&[0x00, 0x08, 0x02, 0xAB]); // 半条（长度行 8 只到 2B 体）
        v.push(flow);
    }
    // 真探测响应骨架（第二道门 高-3：nonce 门在载荷解析之前，固定 nonce 的变异
    // 打不进深层——respond_ex 产真形 + harness 侧 nonce 自取双保险）
    {
        let req = probe::encode_request(probe::TYPE_PING, &[0x0A; 8], 200);
        let resp = probe::respond_ex(
            &req,
            "homeway-rs-fuzz",
            0b11,
            &["127.0.0.1:41641".parse().unwrap(), "192.168.3.12:42661".parse().unwrap()],
        )
        .expect("真探测响应构造");
        v.push(resp);
        // 不带端点列表的老出口形态
        let resp_old = probe::respond_ex(&req, "old", 0b01, &[]).unwrap();
        v.push(resp_old);
    }
    // speedtest 服务端请求 JSON 骨架（第二道门 高-4：parse_request 服务端半边接线）
    v.push(br#"{"role":"recv","warmup_ms":2000,"window_ms":10000}"#.to_vec());
    v.push(br#"{"role":"a,b \"x\"","warmup_ms":1,"window_ms":2}"#.to_vec());
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
        // 真实调用形态（第二道门 低-23）：decode_batch 的消费者拿的是 decode_frame
        // 剥壳后的 payload（type=4 容器），不是整帧——两形态都打
        if let Some((kind, payload)) = frame::decode_frame(&b) {
            if kind == 4 {
                let _ = frame::decode_batch(payload);
            }
        }
        let _ = frame::decode_hint_payload(&b);
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
        // 服务端半边（第二道门 高-4：请求 JSON 手解面——引号感知切分的服务端形态）
        if let Some(req) = homeway_core::speedtest_server::parse_request(&b) {
            assert!(!req.role.is_empty(), "parse_request 不变量：role 空即 None");
            assert!(req.role.len() <= b.len(), "role 长度不可能超过输入（输出上界）");
        }
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
        if let Some(resp) = dp::empty_response(&b) {
            assert!(resp.len() <= b.len(), "empty_response = 12B 头 + question 回显（输出上界）");
        }
        let mut m = b.clone();
        dp::clamp_ttl(&mut m, 60);
        assert!(m.len() == b.len(), "clamp_ttl 原地改写不增字节");
        let _ = dp::count_aaaa(&m);
        let t = dp::truncate(&m, 1232);
        assert!(t.len() <= 1232, "truncate 输出不超过闸值（输出上界）");
        if let Some(f) = dnsface::decode_tcp_frame(&b) {
            assert!(f.len() <= 65535 && f.len() + 2 <= b.len(), "TCP 分帧载荷上界");
        }
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
            // 解析成功 ⇒ 字段与载荷切片自洽（第二道门 低-16：只重复 parse 的守卫是
            // 恒真断言——这里钉「载荷是 total_len 段的后缀、头+L4+载荷不越 total_len」）
            assert!(v.header_len >= 20 && v.header_len <= b.len(), "头长越界");
            assert!(v.total_len <= b.len(), "总长越界：{} > {}", v.total_len, b.len());
            assert!(v.header_len + v.payload.len() <= v.total_len, "头+L4+载荷越出 total_len");
            assert!(b[..v.total_len].ends_with(v.payload), "载荷切片必须是 total_len 段的后缀");
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
        // 第二道门 高-3：nonce 门在载荷解析**之前**（probe.rs:142），固定 nonce 的
        // 输入过不了闸 ⇒ 深层（端点计数/flags/列表段）等于没 fuzz。两条腿：
        // ①固定 nonce（nonce 不匹配拒绝分支）；②nonce 从输入自身取（b ≥13B 时
        // b[5..13]——服务端攻击面本来就是任意源全控字节，nonce 只是语义层校验）。
        let _ = probe::decode_response(&b, &[0x11; 8]);
        if b.len() >= 13 {
            let nonce: [u8; 8] = b[5..13].try_into().expect("已判 13B");
            if let Ok(r) = probe::decode_response(&b, &nonce) {
                assert!(r.endpoints.len() <= probe::MAX_ENDPOINTS, "端点列表超上限");
                // from_utf8_lossy 的替换字符 1B→3B，上界按 3× 载荷段算（输出上界）
                assert!(r.build.len() <= 3 * b.len().saturating_sub(13), "build 串超 lossy 上界");
            }
        }
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
        if let Some(v) = upnp::header_value(&s, "LOCATION") {
            assert!(v.len() <= s.len(), "header_value 输出不可能超过输入（输出上界）");
        }
        if let Some(v) = upnp::xml_tag(&s, "controlURL") {
            assert!(v.len() <= s.len(), "xml_tag 输出不可能超过输入（输出上界）");
        }
        if let Ok((host, port, path)) = upnp::parse_http_url(&s) {
            assert!(host.len() + path.len() + 6 <= s.len(), "URL 三段不可能超过输入（输出上界）");
            assert!(port > 0, "URL 端口为 0");
        }
        let _ = upnp::header_value(&s, "ST");
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
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    };
    let hex_str = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
    // token 正样本：逐字段比对（第二道门 中-11 整改——只数 n_ok 会被「什么都接受」
    // 的解析器骗过；peer_id/secret/endpoints 全对 + 哨兵负例按码拒）
    let p = format!("{}/../../fixtures/vectors/token.json", env!("CARGO_MANIFEST_DIR"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    for c in v["cases"].as_array().unwrap() {
        let tok = c["token"].as_str().unwrap();
        let t = token::decode(tok)
            .unwrap_or_else(|e| panic!("正样本应可解（{}）：{e}", c["name"]));
        let want = &c["decoded"];
        assert_eq!(
            hex_str(t.peer_id.as_bytes()),
            want["peer_id"].as_str().unwrap(),
            "peer_id 逐字段（case={}",
            c["name"]
        );
        assert_eq!(
            hex_str(t.secret.as_bytes()),
            want["secret"].as_str().unwrap(),
            "secret 逐字段（case={}",
            c["name"]
        );
        let weps = want["endpoints"].as_array().unwrap();
        assert_eq!(t.endpoints.len(), weps.len(), "端点数（case={}", c["name"]);
        for (ep, we) in t.endpoints.iter().zip(weps) {
            assert_eq!(ep.addr, we["addr"].as_str().unwrap(), "端点地址（case={}", c["name"]);
            let relay_want = we["relay"].as_bool().unwrap();
            let relay_got = matches!(ep.kind, homeway_core::token::EndpointKind::Relay);
            assert_eq!(relay_got, relay_want, "端点中继位（case={}", c["name"]);
        }
    }
    // token 负例：按期望哨兵码拒绝（unsupported_version/corrupted/malformed 三类）
    for c in v["errors"].as_array().unwrap() {
        let tok = c["input"].as_str().unwrap();
        let err = token::decode(tok)
            .err()
            .unwrap_or_else(|| panic!("负例必须被拒（{}）：{tok}", c["name"]));
        let got = match err {
            token::TokenError::UnsupportedVersion { .. } => "unsupported_version",
            token::TokenError::Corrupted => "corrupted",
            token::TokenError::Malformed { .. } => "malformed",
            _ => "other",
        };
        assert_eq!(got, c["error"].as_str().unwrap(), "负例哨兵码（case={}", c["name"]);
    }
    // STUN 请求骨架往返（stun_sped 向量的首样本）
    let p2 = format!("{}/../../fixtures/vectors/stun_sped.json", env!("CARGO_MANIFEST_DIR"));
    let v2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p2).unwrap()).unwrap();
    let c = &v2["stun"]["requests"][0];
    let mut tx = [0u8; 12];
    tx.copy_from_slice(&hex(c["txid"].as_str().unwrap()));
    assert_eq!(
        homeway_core::server::egress::stun_request(&tx, c["software"].as_str().unwrap()),
        hex(c["wire"].as_str().unwrap()),
        "骨架往返稳定"
    );
}

// ---------- ⑫ term 帧协议（fuzz_term_frames；cargo-fuzz 同名目标） ----------

/// term 种子池（夹具字节：真会话流 + golden 分片帧 + 帧骨架）。
fn term_seeds() -> Vec<Vec<u8>> {
    use homeway_core::term::frames;
    let base = format!("{}/../../fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut v: Vec<Vec<u8>> = Vec::new();
    for name in ["session-cjk.bin", "session-git-log.bin", "session-hexdump.bin"] {
        if let Ok(b) = std::fs::read(format!("{base}/term-vt/{name}")) {
            v.push(b);
        }
    }
    for name in ["session-cjk.bin", "session-styles.bin"] {
        if let Ok(b) = std::fs::read(format!("{base}/surface-golden/{name}")) {
            v.push(b);
        }
    }
    // 帧骨架（真形：HELLO 尾随块 / ATTACHED / STATE / ENDED / RESIZE / INPUT）
    let tail = frames::enc_hello_tail(frames::caps::SURFACE | frames::caps::RAW_TERMINAL, true, "fuzz-1");
    v.push(frames::encode_frame(frames::Op::HELLO, &frames::enc_hello(80, 24, 3, "s", &tail)));
    v.push(frames::encode_frame(frames::Op::ATTACHED, &frames::enc_attached(100, 32, 0x41, 1, 2, "s")));
    v.push(frames::encode_frame(frames::Op::STATE, &frames::enc_state(1, 2, "标题")));
    v.push(frames::encode_frame(frames::Op::ENDED, &frames::enc_ended(-1, "replaced")));
    v.push(frames::encode_frame(frames::Op::RESIZE, &frames::enc_resize(120, 40)));
    v.push(frames::encode_frame(
        frames::Op::INPUT,
        &frames::enc_input(&frames::InputEvent::Key { key: 65, mods: 1, action: 1, text: String::new() }),
    ));
    v.push(frames::encode_frame(frames::Op::ERROR, &frames::enc_error("bad_hello", "x")));
    v
}

/// ⑫ term 帧协议解码族（fuzz_term_frames）：不 panic（帧头/长度域/尾随块全分支）。
#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_term_frames() {
    use homeway_core::term::frames;
    let mut rng = Rng(seed() ^ 0x0c0c);
    let sd = term_seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let _ = frames::read_frame(&mut &b[..]);
        let _ = frames::dec_greeting(&b);
        let _ = frames::dec_hello(&b);
        let _ = frames::dec_hello_tail(&b);
        let _ = frames::dec_create(&b);
        let _ = frames::dec_resize(&b);
        let _ = frames::dec_attached(&b);
        let _ = frames::dec_replay_done(&b);
        let _ = frames::dec_ended(&b);
        let _ = frames::dec_state(&b);
        let _ = frames::dec_error(&b);
        let _ = frames::dec_name(&b);
        let _ = frames::dec_input(&b);
    }
}

// ---------- ⑬ term vt 底座 + 键/鼠标编码（fuzz_term_vt） ----------

/// ⑬ vt 仿真底座（fuzz_term_vt）：不 panic + **F5 运行时哨兵**（每格 symbol ≤127 B）。
#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_term_vt() {
    use homeway_core::term::keyenc;
    use homeway_core::term::vt::SessionVt;
    let mut rng = Rng(seed() ^ 0x0d0d);
    let sd = term_seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        let Ok(mut vt) = SessionVt::new(20, 5, 100) else { continue };
        vt.write_collecting(&b, &mut |_r| {});
        let _ = vt.take_clip_events();
        let _ = vt.update();
        for row in vt.dirty_rows() {
            for c in &row.cells {
                assert!(c.symbol.len() <= 127, "cell symbol 超上限（F5 哨兵）：{} B", c.symbol.len());
            }
        }
        vt.clean();
        let _ = vt.rows();
        let _ = vt.screen_text();
        let _ = vt.plain_text();
        let _ = vt.cursor();
        let _ = vt.modes();
        let _ = vt.scrollbar();
        let _ = vt.rows_at(0, 64);
        let _ = vt.mirror_rows(32);
        let g = |i: usize| b.get(i).copied().unwrap_or(0);
        let _ = vt.encode_key(&keyenc::KeyEvent {
            key: keyenc::Key(u16::from_le_bytes([g(0), g(1)])),
            action: keyenc::KeyAction::from_wire(g(2) % 3).unwrap_or_default(),
            mods: keyenc::Mods(u16::from_le_bytes([g(3), g(4)])),
            text: "",
            composing: g(5) & 1 != 0,
        });
        let _ = vt.encode_mouse(&keyenc::MouseEvent {
            action: keyenc::MouseAction::from_wire(g(6) % 3).unwrap_or_default(),
            button: keyenc::MouseButton(g(7)),
            mods: keyenc::Mods(u16::from_le_bytes([g(8), g(9)])),
            x: u16::from_le_bytes([g(10), g(11)]),
            y: u16::from_le_bytes([g(12), g(13)]),
        });
    }
}

// ---------- ⑭ term surface 体编解码（fuzz_term_codec） ----------

/// ⑭ surface v4 体（fuzz_term_codec）：不 panic + decode→再 encode 字节相等
/// （harness 预检 ≠ 库守卫；库守卫 = F10 的 `guard_dims`/`MAX_GUNZIP_OUT`，由单测钉住）。
#[test]
#[ignore = "fuzz 全量档"]
fn fuzz_term_codec() {
    use homeway_core::term::codec;
    use homeway_core::term::vt;
    const DIM_CAP: u64 = 200_000;
    let mut rng = Rng(seed() ^ 0x0e0e);
    let sd = term_seeds();
    for _ in 0..ITER {
        let b = gen_input(&mut rng, &sd);
        // 自产网格往返（字节相等；decode 派生 width/wraps ⇒ 结构体相等会假红）
        let cols = 1 + (b.first().copied().unwrap_or(0) as usize % 40);
        let rows = 1 + (b.get(1).copied().unwrap_or(0) as usize % 8);
        let mut chunks = b.chunks(4);
        let synth: Vec<vt::Row> = (0..rows)
            .map(|y| vt::Row {
                y: y as u16,
                dirty: false,
                wraps: false,
                cells: (0..cols)
                    .map(|x| {
                        let sym: String = chunks
                            .next()
                            .unwrap_or(&[])
                            .iter()
                            .filter(|c| c.is_ascii_graphic())
                            .map(|c| *c as char)
                            .collect();
                        vt::Cell {
                            symbol: if sym.is_empty() { "x".to_string() } else { sym },
                            width: 1,
                            skip: (x + y) % 5 == 0,
                            fg: vt::Color::Palette((x % 256) as u8),
                            bg: vt::Color::None,
                            attr: ((x * 7 + y * 13) & 0x0fff) as u16,
                        }
                    })
                    .collect(),
            })
            .collect();
        let grid = codec::encode_grid(cols as u16, rows as u16, &synth);
        let (c, r, decoded) = codec::decode_grid(&grid).expect("自产网格必可解");
        assert_eq!(codec::encode_grid(c, r, &decoded), grid, "往返字节相等");
        let rows_enc = codec::encode_rows(&synth);
        let dr = codec::decode_rows(&rows_enc, rows, cols).expect("自产行序列必可解");
        assert_eq!(codec::encode_rows(&dr), rows_enc, "行序列往返字节相等");
        // 原始输入解析面（按布局派生维度预检）
        if b.len() >= 5 {
            let gc = u16::from_le_bytes([b[1], b[2]]) as u64;
            let gr = u16::from_le_bytes([b[3], b[4]]) as u64;
            if gc.saturating_mul(gr) <= DIM_CAP {
                let _ = codec::decode_grid(&b);
            }
        }
        if b.len() >= 4 {
            let rc = u16::from_le_bytes([b[0], b[1]]) as u64;
            let rn = u16::from_le_bytes([b[2], b[3]]) as u64;
            if rc.saturating_mul(rn) <= DIM_CAP {
                let _ = codec::decode_rows(&b[4..], rn as usize, rc as usize);
            }
        }
        if b.len() >= 9 {
            let bc = u16::from_le_bytes([b[5], b[6]]) as u64;
            let br = u16::from_le_bytes([b[7], b[8]]) as u64;
            if bc.saturating_mul(br) <= DIM_CAP {
                let _ = codec::dec_snapshot_body(&b);
                let _ = codec::dec_diff_body(&b);
            }
        }
        let _ = codec::gunzip_bytes(&b);
        let _ = codec::dec_fetch_rows_req(&b);
        let _ = codec::dec_fetch_rows_reply(&b);
        let _ = codec::dec_fragment(&b);
        let _ = codec::dec_theme(&b);
        let _ = codec::dec_clipboard(&b);
        let _ = codec::dec_notify(&b);
        let mut asm = codec::FragAssembler::default();
        for frag in b.chunks(64) {
            let _ = asm.push(frag);
        }
        let _ = codec::fragment_payload(&b);
    }
}

// ---------- ⑪ 迭代预算（quick 档可见——第二道门 低-15：≥100k 的量级在仓库内可断言） ----------

#[test]
fn fuzz_iteration_budget() {
    // 十二个重放目标的量级门（编译期断言——ITER 是常量；下调预算必须显式改这里）：
    // cargo test --ignored 跑的就是这个 ITER，R5 判据口径 ≥100k。
    // Q-D 批新增 ⑫⑬⑭（term 帧/vt/codec）——codec 目标带自产网格往返，
    // 全量档预计 +2-4 min（见 tools/ci-local.sh 注释）。
    const _: () = assert!(ITER >= 100_000, "fuzz 重放轨预算 < 100k（R5 判据口径）");
}
