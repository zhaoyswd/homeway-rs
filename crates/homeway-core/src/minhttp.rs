//! 极小同步 HTTPS GET 客户端（`homeway update` 频道专用，2026-10-23 批）。
//!
//! 服务对象 = update 的两条腿：GitHub API 查询（小 JSON）与 Release 产物下载
//! （tar.gz / SHA256SUMS，数 MiB 量级**进内存**——CLI 一次性流程，不做流式落盘）。
//! 与 DoH（`server/doh.rs`）是**并行的两条独立同步 TLS 腿**，有意不复用也不改动它：
//! DoH 是钉卡 + POST + 64KiB 上限的生产 DNS 纪律件（r5 契约），本件是系统 DNS +
//! GET + 重定向 + 调用方给体上限的更新频道件；HTTP framing 语义同源但约束不同，
//! 为共享而重构生产 DNS 路径的收益不抵风险（两件的偏差已在此登记，收敛留待将来
//! 真有第三个消费者时再做）。
//!
//! 纪律：
//! - **纯同步**（无 async/tokio——check-quic-isolation ① 的同步 rustls 白名单第二件）；
//! - 每次调用一个**绝对截止**（`Instant`），DNS/连接/握手/收包共享同一预算；
//! - 重定向 ≤5 跳且只跟随 `https`；响应头上限 64KiB；体上限由调用方 `cap` 给。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Instant;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// 固定 UA（GitHub 的 API 与下载端都强制要求 User-Agent）。
const UA: &str = "homeway-update";
/// 重定向跳数上限（github.com → objects.githubusercontent.com 等签名跳转链）。
const HOP_MAX: usize = 5;
/// 响应头（状态行 + 头域）上限——与 DoH 同值。
const HEAD_CAP: usize = 64 * 1024;
/// 单行（chunk 尺寸行 / 头行 / trailer 行）上限。
const LINE_CAP: usize = 1024;

/// GET 成功应答（最终跳的 status + 全量体；3xx 无 Location 也原样返回，调用方自判）。
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 同步 HTTPS GET 的失败面（thiserror；传输层——HTTP 4xx/5xx 属于成功应答的 status）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HttpError {
    #[error("DNS 解析 {0} 失败")]
    Dns(String),
    #[error("TCP 连接失败：{0}")]
    Connect(String),
    #[error("TLS 失败：{0}")]
    Tls(String),
    #[error("HTTP 收发失败：{0}")]
    Io(String),
    #[error("预算耗尽（截止前未完成）")]
    Deadline,
    #[error("响应头超限（> 64 KiB）")]
    HeadTooLarge,
    #[error("响应体超限（> {0} 字节）")]
    BodyTooLarge(usize),
    #[error("重定向超过 {0} 跳")]
    TooManyRedirects(usize),
    #[error("重定向目标不是 https：{0}")]
    InsecureRedirect(String),
    #[error("响应不完整（连接早断）")]
    Truncated,
}

/// https URL 拆 (host, path)。只认 `https`（重定向跟随的安全前提）；无路径按 `/`；
/// 显式 `:443` 端口剥除（本客户端恒连 443）。其它形态（http/无 scheme/带端口）→ None。
pub fn https_parts(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_owned()),
    };
    if host.is_empty() {
        return None;
    }
    let host = host.strip_suffix(":443").unwrap_or(host);
    if host.contains(':') {
        return None; // 非默认端口：本频道不该出现（GitHub 族恒 443）
    }
    Some((host.to_owned(), path))
}

/// 单次 GET（含 ≤5 跳重定向跟随）。`cap` = 体上限（API JSON 用小值、产物用大值）。
pub fn get(
    host: &str,
    path: &str,
    accept: Option<&str>,
    deadline: Instant,
    cap: usize,
) -> Result<Response, HttpError> {
    let mut host = host.to_owned();
    let mut path = path.to_owned();
    let mut hops = 0usize;
    loop {
        let mut body = Vec::new();
        let head = request_once(&host, &path, accept, deadline, cap, &mut body)?;
        match next_hop(head.status, head.location.as_deref(), hops)? {
            Some((h, p)) => {
                host = h;
                path = p;
                hops += 1;
            }
            None => return Ok(Response { status: head.status, body }),
        }
    }
}

/// 重定向决策（纯函数，供单测）：非 3xx / 3xx 无 Location → 原样返回（None）；
/// 跳数耗尽 → `TooManyRedirects`；目标非 https（或拆不开）→ `InsecureRedirect`。
fn next_hop(status: u16, location: Option<&str>, hops: usize) -> Result<Option<(String, String)>, HttpError> {
    if !(300..=399).contains(&status) {
        return Ok(None);
    }
    let Some(loc) = location else { return Ok(None) };
    if hops >= HOP_MAX {
        return Err(HttpError::TooManyRedirects(HOP_MAX));
    }
    match https_parts(loc) {
        Some((h, p)) => Ok(Some((h, p))),
        None => Err(HttpError::InsecureRedirect(loc.to_owned())),
    }
}

/// 响应头里 framing/跳转要用的三件事。
#[derive(Debug)]
struct Head {
    status: u16,
    location: Option<String>,
    content_length: Option<usize>,
    chunked: bool,
}

fn parse_head(head: &str) -> Result<Head, HttpError> {
    let bad = |what: &str| HttpError::Io(format!("响应头非法：{what}"));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| bad("空头"))?;
    let mut tok = status_line.split_whitespace();
    let proto = tok.next().ok_or_else(|| bad("状态行缺协议"))?;
    if !proto.starts_with("HTTP/") {
        return Err(bad("状态行不是 HTTP"));
    }
    let status: u16 = tok
        .next()
        .ok_or_else(|| bad("状态行缺码"))?
        .parse()
        .map_err(|_| bad("状态码非数字"))?;
    let mut out = Head { status, location: None, content_length: None, chunked: false };
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let (k, v) = l.split_once(':').ok_or_else(|| bad("头行无冒号"))?;
        let v = v.trim();
        if k.eq_ignore_ascii_case("content-length") {
            if out.content_length.is_some() {
                return Err(bad("content-length 重复"));
            }
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad("content-length 非法"));
            }
            out.content_length = Some(v.parse().map_err(|_| bad("content-length 溢出"))?);
        } else if k.eq_ignore_ascii_case("transfer-encoding") {
            if out.chunked || !v.eq_ignore_ascii_case("chunked") {
                return Err(bad("transfer-encoding 只认 chunked"));
            }
            out.chunked = true;
        } else if k.eq_ignore_ascii_case("location") && out.location.is_none() {
            out.location = Some(v.to_owned());
        }
    }
    if out.chunked && out.content_length.is_some() {
        return Err(bad("content-length 与 chunked 并存"));
    }
    Ok(out)
}

/// rustls 客户端配置（webpki-roots 根束——与 DoH 同源同款；无客户端证书）。
fn client_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

fn request_once(
    host: &str,
    path: &str,
    accept: Option<&str>,
    deadline: Instant,
    cap: usize,
    body: &mut Vec<u8>,
) -> Result<Head, HttpError> {
    let remaining = || deadline.saturating_duration_since(Instant::now());
    if remaining().is_zero() {
        return Err(HttpError::Deadline);
    }
    let addrs: Vec<_> = (host, 443u16)
        .to_socket_addrs()
        .map_err(|e| HttpError::Dns(e.to_string()))?
        .collect();
    if addrs.is_empty() {
        return Err(HttpError::Dns("无地址".into()));
    }
    let mut last: Option<String> = None;
    let mut tcp: Option<TcpStream> = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, remaining()) {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = Some(format!("{a}: {e}")),
        }
        if remaining().is_zero() {
            return Err(HttpError::Deadline);
        }
    }
    let tcp = tcp.ok_or_else(|| HttpError::Connect(last.unwrap_or_else(|| "全部候选失败".into())))?;
    tcp.set_nodelay(true).ok();
    // 每个方向的 syscall 都不越过总截止（超时返回 WouldBlock/TimedOut 后由
    // 读循环的 Instant 检查收口成 Deadline，不误报 Io）。
    let _ = tcp.set_read_timeout(Some(remaining()));
    let _ = tcp.set_write_timeout(Some(remaining()));
    let tls = ClientConnection::new(
        std::sync::Arc::new(client_config()),
        ServerName::try_from(host.to_owned()).map_err(|e| HttpError::Tls(e.to_string()))?,
    )
    .map_err(|e| HttpError::Tls(e.to_string()))?;
    let mut s = StreamOwned::new(tls, tcp);
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {UA}\r\nAccept: {}\r\nConnection: close\r\n\r\n",
        accept.unwrap_or("*/*"),
    );
    s.write_all(req.as_bytes())
        .and_then(|()| s.flush())
        .map_err(|e| HttpError::Tls(e.to_string()))?;
    let mut src = Src { r: s, pending: Vec::new(), pos: 0, deadline };
    read_response(&mut src, cap, body)
}

/// 消费面：`pending[pos..]` 是已读未消费的字节；`fill` 追加读。
/// 泛型 over `Read` —— 生产 = `StreamOwned`（TLS），测试 = 内存假流（framing 单测）。
struct Src<R: Read> {
    r: R,
    pending: Vec<u8>,
    pos: usize,
    deadline: Instant,
}

impl<R: Read> Src<R> {
    /// 追加读一块；EOF 返回 Ok(false)。
    fn fill(&mut self) -> Result<bool, HttpError> {
        if Instant::now() >= self.deadline {
            return Err(HttpError::Deadline);
        }
        let mut tmp = [0u8; 8192];
        let n = self.r.read(&mut tmp).map_err(|e| HttpError::Io(e.to_string()))?;
        if Instant::now() >= self.deadline {
            return Err(HttpError::Deadline);
        }
        if n == 0 {
            return Ok(false);
        }
        self.pending.extend_from_slice(&tmp[..n]);
        Ok(true)
    }

    fn unread(&self) -> &[u8] {
        &self.pending[self.pos..]
    }

    fn compact(&mut self) {
        if self.pos > 64 * 1024 {
            self.pending.drain(..self.pos);
            self.pos = 0;
        }
    }

    /// 取恰好 `n` 字节进 `out`（超 `cap` → BodyTooLarge；EOF → Truncated）。
    fn take(&mut self, out: &mut Vec<u8>, n: usize, cap: usize) -> Result<(), HttpError> {
        if out.len().saturating_add(n) > cap {
            return Err(HttpError::BodyTooLarge(cap));
        }
        while self.unread().len() < n {
            if !self.fill()? {
                return Err(HttpError::Truncated);
            }
        }
        let pos = self.pos;
        out.extend_from_slice(&self.pending[pos..pos + n]);
        self.pos += n;
        self.compact();
        Ok(())
    }

    /// 取一行（不含结尾 CRLF；行内允许裸 `\r` 不计——按 `\n` 切，剥尾 `\r`）。
    /// 行长超 `LINE_CAP` → Io（尺寸行/头行/trailer 都不该长）。
    fn take_line(&mut self) -> Result<Vec<u8>, HttpError> {
        loop {
            if let Some(nl) = self.unread().iter().position(|&b| b == b'\n') {
                if nl >= LINE_CAP {
                    return Err(HttpError::Io("行超限".into()));
                }
                let pos = self.pos;
                let mut line = self.pending[pos..pos + nl].to_vec();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.pos += nl + 1;
                self.compact();
                return Ok(line);
            }
            if self.unread().len() >= LINE_CAP {
                return Err(HttpError::Io("行超限（无换行）".into()));
            }
            if !self.fill()? {
                return Err(HttpError::Truncated);
            }
        }
    }
}

/// 读完整响应（头 + 体）到 `body`，返回头三件事。
fn read_response<R: Read>(src: &mut Src<R>, cap: usize, body: &mut Vec<u8>) -> Result<Head, HttpError> {
    // 头：直到 CRLFCRLF。
    let head_end = loop {
        if let Some(p) = find_crlfcrlf(src.unread()) {
            break src.pos + p;
        }
        if src.unread().len() > HEAD_CAP {
            return Err(HttpError::HeadTooLarge);
        }
        if !src.fill()? {
            return Err(HttpError::Truncated);
        }
    };
    let head_str = std::str::from_utf8(&src.pending[src.pos..head_end])
        .map_err(|_| HttpError::Io("响应头非 UTF-8".into()))?
        .to_owned();
    src.pos = head_end + 4;
    let head = parse_head(&head_str)?;
    if let Some(n) = head.content_length {
        src.take(body, n, cap)?;
        return Ok(head);
    }
    if head.chunked {
        read_chunked(src, cap, body)?;
        return Ok(head);
    }
    // 无 CL 无 TE：按 Connection: close 读到 EOF。
    while src.fill()? {
        if src.unread().len() > cap {
            return Err(HttpError::BodyTooLarge(cap));
        }
        body.extend_from_slice(src.unread());
        src.pos = src.pending.len();
        if body.len() > cap {
            return Err(HttpError::BodyTooLarge(cap));
        }
    }
    body.extend_from_slice(src.unread());
    src.pos = src.pending.len();
    Ok(head)
}

fn read_chunked<R: Read>(src: &mut Src<R>, cap: usize, body: &mut Vec<u8>) -> Result<(), HttpError> {
    loop {
        let line = src.take_line()?;
        let line = String::from_utf8_lossy(&line).into_owned();
        let size_tok = line.split(';').next().unwrap_or("").trim();
        let n = usize::from_str_radix(size_tok, 16)
            .map_err(|_| HttpError::Io(format!("chunk 尺寸行非法：{size_tok:?}")))?;
        if n == 0 {
            // trailer 区：读到空行收尾。
            loop {
                if src.take_line()?.is_empty() {
                    return Ok(());
                }
            }
        }
        src.take(body, n, cap)?;
        let mut crlf = Vec::new();
        src.take(&mut crlf, 2, usize::MAX)?;
        if crlf != b"\r\n" {
            return Err(HttpError::Io("chunk 后缺 CRLF".into()));
        }
    }
}

fn find_crlfcrlf(hay: &[u8]) -> Option<usize> {
    hay.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later() -> Instant {
        Instant::now() + std::time::Duration::from_secs(10)
    }

    /// 按给定分片逐块吐字节的假流（模拟慢网络下的分帧到达）。
    struct Trickle(Vec<Vec<u8>>, usize);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            while self.0.len() > self.1 && self.0[self.1].is_empty() {
                self.1 += 1;
            }
            let Some(chunk) = self.0.get(self.1) else { return Ok(0) };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            if n == chunk.len() {
                self.1 += 1;
            } else {
                self.0[self.1] = chunk[n..].to_vec();
            }
            Ok(n)
        }
    }

    fn src_of(parts: &[&[u8]]) -> Src<Trickle> {
        Src {
            r: Trickle(parts.iter().map(|p| p.to_vec()).collect(), 0),
            pending: Vec::new(),
            pos: 0,
            deadline: later(),
        }
    }

    #[test]
    fn content_length_body_across_trickled_reads() {
        let mut src = src_of(&[
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhe".as_slice(),
            b"ll".as_slice(),
            b"o".as_slice(),
        ]);
        let mut body = Vec::new();
        let head = read_response(&mut src, 1024, &mut body).unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(body, b"hello");
    }

    #[test]
    fn chunked_body_with_trailers() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Sum: 9\r\n\r\n";
        let mut src = src_of(&[raw.as_slice()]);
        let mut body = Vec::new();
        let head = read_response(&mut src, 1024, &mut body).unwrap();
        assert!(head.chunked);
        assert_eq!(body, b"Wikipedia");
    }

    #[test]
    fn eof_framed_body_without_length() {
        let raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ntail-bytes";
        let mut src = src_of(&[raw.as_slice()]);
        let mut body = Vec::new();
        read_response(&mut src, 1024, &mut body).unwrap();
        assert_eq!(body, b"tail-bytes");
    }

    #[test]
    fn truncated_body_is_reported() {
        let mut src = src_of(&[b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".as_slice()]);
        let mut body = Vec::new();
        assert!(matches!(
            read_response(&mut src, 1024, &mut body),
            Err(HttpError::Truncated)
        ));
    }

    #[test]
    fn body_cap_enforced() {
        let mut src = src_of(&[b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789".as_slice()]);
        let mut body = Vec::new();
        assert!(matches!(
            read_response(&mut src, 10, &mut body),
            Err(HttpError::BodyTooLarge(10))
        ));
    }

    #[test]
    fn head_cap_enforced() {
        let mut huge = Vec::new();
        huge.extend_from_slice(b"HTTP/1.1 200 OK\r\nX-Big: ");
        huge.extend(std::iter::repeat_n(b'a', HEAD_CAP + 1));
        let mut src = src_of(&[huge.as_slice()]);
        let mut body = Vec::new();
        assert!(matches!(
            read_response(&mut src, 1024, &mut body),
            Err(HttpError::HeadTooLarge)
        ));
    }

    #[test]
    fn head_rejects_ambiguous_framing_and_bad_lines() {
        assert!(parse_head("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n").is_err());
        assert!(parse_head("HTTP/1.1 200 OK\r\nContent-Length: 3\r\nContent-Length: 4\r\n").is_err());
        assert!(parse_head("HTTP/1.1 200 OK\r\nContent-Length: abc\r\n").is_err());
        assert!(parse_head("garbage\r\n").is_err());
        let ok = parse_head("HTTP/1.1 302 Found\r\nLocation: https://x.example/a/b\r\nContent-Length: 0\r\n").unwrap();
        assert_eq!(ok.status, 302);
        assert_eq!(ok.location.as_deref(), Some("https://x.example/a/b"));
    }

    #[test]
    fn https_parts_forms() {
        assert_eq!(
            https_parts("https://github.com/a/b/c.tar.gz"),
            Some(("github.com".into(), "/a/b/c.tar.gz".into()))
        );
        assert_eq!(https_parts("https://example.com"), Some(("example.com".into(), "/".into())));
        assert_eq!(https_parts("https://example.com:443/x"), Some(("example.com".into(), "/x".into())));
        assert_eq!(https_parts("https://example.com:8443/x"), None);
        assert_eq!(https_parts("http://example.com/x"), None);
        assert_eq!(https_parts("example.com/x"), None);
    }

    #[test]
    fn next_hop_policy() {
        // 非 3xx / 3xx 无 Location → 原样。
        assert!(next_hop(200, Some("https://a/"), 0).unwrap().is_none());
        assert!(next_hop(302, None, 0).unwrap().is_none());
        // 正常跳。
        let (h, p) = next_hop(302, Some("https://objects.example/f?sig=1"), 0).unwrap().unwrap();
        assert_eq!(h, "objects.example");
        assert_eq!(p, "/f?sig=1");
        // 跳数耗尽 / 非 https。
        assert!(matches!(
            next_hop(302, Some("https://a/"), HOP_MAX),
            Err(HttpError::TooManyRedirects(HOP_MAX))
        ));
        assert!(matches!(
            next_hop(301, Some("http://a/"), 0),
            Err(HttpError::InsecureRedirect(_))
        ));
    }

    #[test]
    fn deadline_is_enforced_on_idle_source() {
        let mut src = Src { r: Trickle(vec![], 0), pending: Vec::new(), pos: 0, deadline: Instant::now() };
        let mut body = Vec::new();
        assert!(matches!(read_response(&mut src, 10, &mut body), Err(HttpError::Deadline)));
    }

    /// 真网冒烟（默认忽略；`cargo test -p homeway-core -- --ignored` 手动跑）：
    /// 拉 GitHub API 的最新 release 元数据——CI 无外网/限流环境下不作为门。
    #[test]
    #[ignore = "真网依赖：手动跑"]
    fn github_latest_release_over_real_tls() {
        let resp = get(
            "api.github.com",
            "/repos/zhaoyswd/homeway-rs/releases/latest",
            Some("application/vnd.github+json"),
            Instant::now() + std::time::Duration::from_secs(30),
            2 << 20,
        )
        .unwrap();
        assert_eq!(resp.status, 200);
        let v: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert!(v["tag_name"].as_str().unwrap().starts_with('v'));
    }
}
