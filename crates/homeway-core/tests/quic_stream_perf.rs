//! M3 S8 门槛：**服务流吞吐**（设计 §7/§15-3）——files 下载吞吐（服务流经
//! `STREAM[tag=files]`）。
//!
//! **M5 C3 单臂化（设计 §9.2 默认 (c)）**：原 A/B 对照臂（`transport=wg`：WG 会隖 →
//! intercept 豁免 → UDS）随 WG 面退役 ⇒ 本用例只跑 QUIC 臂（相对门槛的**参照臂消失**
//! 已登记；绝对读数继续入册）。
//!
//! 与既有 e2e 的关系：本用例复用 `[e2e5]`（App 核形态 + 真世代 + 隧道桥 + 真 FilesServer）
//! 的同一条产品路径，只把「list」换成「download 大文件 + 计时」。
//!
//! 驱动：`tools/m3-s8-perf.sh`（起本地私有出口 → 灌 token → 逐臂跑本用例）。
//! 读数（stdout 一行）：`[perf] transport=… file=… bytes=… secs=… mibps=…`。
//!
//! 纪律：本地私有出口实例；不碰现役出口；读数落 `/tmp/m3s8-res/perf/`（仓外）。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::time::{Duration, Instant};

use homeway_core::facade::bridge_host::bridge_client_auth;
use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;

const WAIT: Duration = Duration::from_secs(25);

/// 读一行（`\n` 结尾；**缓冲读**——行后的帧字节留在同一 BufReader 里，不得越读丢弃）。
fn read_line(r: &mut BufReader<UnixStream>, wait: Duration) -> Vec<u8> {
    r.get_mut().set_read_timeout(Some(wait)).ok();
    let mut acc: Vec<u8> = Vec::new();
    let _ = r.read_until(b'\n', &mut acc);
    acc
}

/// 读满 n 字节（帧面用；超时/断开 ⇒ Err）。
fn read_exact(r: &mut BufReader<UnixStream>, n: usize, wait: Duration) -> std::io::Result<Vec<u8>> {
    r.get_mut().set_read_timeout(Some(wait))?;
    let mut out = vec![0u8; n];
    r.read_exact(&mut out)?;
    Ok(out)
}

#[test]
#[ignore = "性能读数：需本地出口在跑（tools/m3-s8-perf.sh 驱动）"]
fn stream_files_download_throughput_by_bearer() {
    let token = std::env::var("HOMEWAY_PERF_TOKEN").expect("须给 HOMEWAY_PERF_TOKEN");
    // M5 C3：WG 参照臂退役——`HOMEWAY_PERF_TRANSPORT` 只接受 `quic`（旧脚本给 `wg`
    // 一律 fail-fast，不静默出空读数）。
    if let Ok(t) = std::env::var("HOMEWAY_PERF_TRANSPORT") {
        assert_eq!(t, "quic", "WG 参照臂已随 WG 面退役（M5 C3；只支持 quic）");
    }
    let transport = "quic";
    let file = std::env::var("HOMEWAY_PERF_FILE").unwrap_or_else(|_| "perf-64m.bin".into());
    let expect_bytes: usize = std::env::var("HOMEWAY_PERF_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let dir = std::env::temp_dir().join(format!("hw-m3s8-perf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token}","out":"{}","identityDir":"{}"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = std::sync::Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"ready\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"ready\""), "世代须 ready");

    let (tun, _peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(core.tun_attach(tun.as_raw_fd(), 1280), 0, "attach 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"attached\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"attached\""), "世代须 attached");

    let log = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(log.contains("quic: 隧道侧就绪（L3 直通；"), "岛须在场：{log}");
    assert!(!log.contains("回落 WG"), "单承载后不得有回落话术：{log}");

    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    let auth = v["bridgeAuth"].as_str().unwrap_or_default().to_owned();
    let files_sock = v["bridgeFilesSock"].as_str().unwrap_or_default().to_owned();
    assert!(!auth.is_empty() && !files_sock.is_empty(), "桥须在场：{v}");

    let c = UnixStream::connect(&files_sock).expect("连 files 桥");
    let mut c = BufReader::new(c);
    bridge_client_auth(c.get_mut(), &auth).expect("桥鉴权首包可写");
    let greet = read_line(&mut c, WAIT);
    assert!(greet.ends_with(b"\n"), "问候行须完整（承载={transport}）");
    let g: serde_json::Value = serde_json::from_slice(&greet).expect("问候行是 JSON");
    assert_eq!(g["ok"], serde_json::json!(true), "问候：{g}");

    // 下载：请求行 → 应答行（带 size）→ 4B BE 长度前缀帧流 → 0 长度终止帧
    let mut req = format!(r#"{{"op":"download","path":"{file}"}}"#).into_bytes();
    req.push(b'\n');
    // 计时起点 = 请求行写完（数据面由此开始搬字节）
    c.get_mut().write_all(&req).expect("写 download 请求行");
    let t0 = Instant::now();
    let resp = read_line(&mut c, WAIT);
    let r: serde_json::Value = serde_json::from_slice(&resp).expect("download 应答是 JSON");
    assert_eq!(r["ok"], serde_json::json!(true), "download 应答：{r}");
    let mut total = 0usize;
    loop {
        let hdr = read_exact(&mut c, 4, WAIT).expect("读帧头");
        let n = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        if n == 0 {
            break;
        }
        let chunk = read_exact(&mut c, n, WAIT).expect("读帧体");
        total += chunk.len();
    }
    let secs = t0.elapsed().as_secs_f64();
    let mibps = (total as f64 / (1024.0 * 1024.0)) / secs.max(1e-9);
    if expect_bytes > 0 {
        assert_eq!(total, expect_bytes, "下载字节数须与源文件一致");
    }
    println!(
        "[perf] transport={transport} file={file} bytes={total} secs={secs:.3} mibps={mibps:.2}"
    );
    drop(c);
    let _ = core.tun_stop();
}

/// 单条 files 下载（**自成一体的连接一命令流**：桥鉴权 → 问候 → 请求行 → 帧到终止帧）。
/// 返回（字节数，计时秒数）；计时窗 = 请求行写完 → 终止帧读尽。
fn download_once(
    files_sock: &str,
    auth: &str,
    file: &str,
    wait: Duration,
) -> std::io::Result<(usize, f64)> {
    let c = UnixStream::connect(files_sock)?;
    let mut c = BufReader::new(c);
    bridge_client_auth(c.get_mut(), auth)?;
    let greet = read_line(&mut c, wait);
    assert!(greet.ends_with(b"\n"), "问候行须完整");
    let mut req = format!(r#"{{"op":"download","path":"{file}"}}"#).into_bytes();
    req.push(b'\n');
    c.get_mut().write_all(&req)?;
    let t0 = Instant::now();
    let resp = read_line(&mut c, wait);
    let r: serde_json::Value = serde_json::from_slice(&resp).expect("download 应答是 JSON");
    assert_eq!(r["ok"], serde_json::json!(true), "download 应答：{r}");
    let mut total = 0usize;
    loop {
        let hdr = read_exact(&mut c, 4, wait)?;
        let n = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        if n == 0 {
            break;
        }
        let chunk = read_exact(&mut c, n, wait)?;
        total += chunk.len();
    }
    Ok((total, t0.elapsed().as_secs_f64()))
}

/// **S9 复测臂：多流并行 bulk 下载**（真机 speedtest 的同规格形态 = 同一连接上 N 条
/// `STREAM[tag=files]` 并行推字节；也是「出口进程死亡」复现的仪器——`HOMEWAY_QUIC_*`
/// 窗口整改后必须确认**聚合**吞吐与出口存活都不退化）。
///
/// env：`HOMEWAY_PERF_PARALLEL`（流数，缺省 4）/ `HOMEWAY_PERF_ROUNDS`（轮数，缺省 1）/
/// `HOMEWAY_PERF_BYTES`（每流期望字节；>0 时断言逐流字节数）/ `HOMEWAY_PERF_FILE`。
/// 读数一行：`[perf-par] streams=… bytes_each=… total=… secs=… mibps=… per=[…]`。
#[test]
#[ignore = "性能读数：需本地出口在跑（tools/m3-s9-bulk.sh 驱动）"]
fn stream_files_parallel_download() {
    let token = std::env::var("HOMEWAY_PERF_TOKEN").expect("须给 HOMEWAY_PERF_TOKEN");
    if let Ok(t) = std::env::var("HOMEWAY_PERF_TRANSPORT") {
        assert_eq!(t, "quic", "WG 参照臂已随 WG 面退役（M5 C3；只支持 quic）");
    }
    let file = std::env::var("HOMEWAY_PERF_FILE").unwrap_or_else(|_| "perf-64m.bin".into());
    let streams: usize = std::env::var("HOMEWAY_PERF_PARALLEL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let rounds: usize = std::env::var("HOMEWAY_PERF_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let expect_bytes: usize = std::env::var("HOMEWAY_PERF_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let dir = std::env::temp_dir().join(format!("hw-m3s9-par-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token}","out":"{}","identityDir":"{}"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = std::sync::Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"ready\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"ready\""), "世代须 ready");
    let (tun, _peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(core.tun_attach(tun.as_raw_fd(), 1280), 0, "attach 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"attached\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"attached\""), "世代须 attached");
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    let auth = v["bridgeAuth"].as_str().unwrap_or_default().to_owned();
    let files_sock = v["bridgeFilesSock"].as_str().unwrap_or_default().to_owned();
    assert!(!auth.is_empty() && !files_sock.is_empty(), "桥须在场：{v}");

    for round in 1..=rounds {
        let t0 = Instant::now();
        let mut handles = Vec::with_capacity(streams);
        for i in 0..streams {
            let (sock, a, f) = (files_sock.clone(), auth.clone(), file.clone());
            handles.push(std::thread::spawn(move || {
                let r = download_once(&sock, &a, &f, WAIT);
                (i, r)
            }));
        }
        let mut per = Vec::with_capacity(streams);
        let mut total = 0usize;
        for h in handles {
            let (i, r) = h.join().expect("下载线程不 panic");
            let (bytes, secs) = r.unwrap_or_else(|e| panic!("流 {i} 下载失败：{e}"));
            assert!(expect_bytes == 0 || bytes == expect_bytes, "流 {i} 字节 {bytes} ≠ {expect_bytes}");
            per.push(bytes as f64 / (1024.0 * 1024.0) / secs.max(1e-9));
            total += bytes;
        }
        let secs = t0.elapsed().as_secs_f64();
        let mibps = (total as f64 / (1024.0 * 1024.0)) / secs.max(1e-9);
        let per_s: Vec<String> = per.iter().map(|v| format!("{v:.2}")).collect();
        println!(
            "[perf-par] round={round} streams={streams} bytes_each={} total={total} secs={secs:.3} mibps={mibps:.2} per=[{}]",
            total / streams,
            per_s.join(",")
        );
    }
    let _ = core.tun_stop();
}
