# Q-K 设计文档 — 出口侧有界 IPv4 分片重组

- **批**：Q-K（`ROADMAP.md` §「Q 批之后的批次与待裁决」条目：「出口侧有界 IPv4 分片重组」）
- **棒次**：第 1 棒（设计）；本文件是设计文档，**不含产品代码**。
- **基线**：`main` @ `72f1325`（Q-F-B 收口 commit）
- **日期**：2026-10-08
- **版本**：v2（设计门 r28 意见已逐条并入；v1 的 R9 / F5-a / T24 三处由门内【高-1】推翻并订正）
- **判据政策**：`docs/INTEROP-CRITERIA.md` §「判据变更记录」——本批登记见 §7。
- **两门**：设计门 = §9（已过）；代码门由第 2 棒跑，不在本文件范围。

---

## 0. 复验方法（可复现）

| 手段 | 具体动作 |
|---|---|
| 源码定位 | 回读 `crates/homeway-core/src/server/intercept/{mod.rs,nat.rs}`、`crates/homeway-core/src/wgcore/{mod.rs,stackb.rs}`、根 `Cargo.toml`、`server/{engine.rs,device.rs}`、`daemon/proto.rs`、`crates/homeway-cli/src/unified_cli.rs` |
| 依赖源码 | 直接读 cargo registry 内的 `smoltcp-0.14.0`（`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/smoltcp-0.14.0`），含 `build.rs` 的 `CONFIGS` 默认表、`src/iface/{interface/mod.rs,interface/ipv4.rs,fragmentation.rs,packet.rs}`、`src/wire/udp.rs`、`src/phy/mod.rs` |
| **实证探针** | 在 `stackb.rs` 的 `mod tests` 临时加探针测试（客户端栈发不同长度 UDP，`drain_tx` 数线上包），`cargo test -p homeway-core --lib tmp_probe_… -- --nocapture` 跑出实测阈值；**跑完立刻 `git checkout` 还原，工作区已确认干净**（输出见 §1-R3/R4） |
| Go 侧 oracle | **只读**读 `baseline/homeway/pkg/{intercept,wgnet}`；gVisor 源码 = Go module cache `gvisor.dev/gvisor@v0.0.0-20250503011706-39ed1f5ac29c`（`baseline/homeway/go.mod:15` 钉同一版本） |
| 外部评审 | dsh headless 设计门（§9），评审者独立复核了本文件全部证据链并推翻了三处结论 |
| 边界纪律 | 未改 `baseline/`、未碰 `~/Documents/projects/{homeway,tier}`、未碰生产出口、未发 tag/PR |

---

## 1. 复验结果表（证据链逐条）

> 「订正」= 与主会话转述不符、或**本文件 v1 写错后被设计门推翻**（R9/R15）。

### R1 — smoltcp 的 IPv4 分片能力被编译进来：**成立**

- `Cargo.toml:30`：`smoltcp = { version = "0.14", features = ["socket-tcp-cubic", "socket-tcp-reno"] }`——**未写 `default-features = false`**。
- smoltcp `Cargo.toml` 的 `default = [ … "proto-ipv4-fragmentation" … ]`，且 `proto-ipv4-fragmentation = ["proto-ipv4", "_proto-fragmentation"]`。
- 关键结构（`src/iface/fragmentation.rs` 的 `FragmentsBuffer`/`Fragmenter`/`PacketAssemblerSet`）全部 `#[cfg(feature = "_proto-fragmentation")]`。
- **额外核对**：全仓 `grep -rn "SMOLTCP_\|fragmentation-buffer-size\|reassembly-buffer"` **零命中** ⇒ 没有任何特性位/环境变量调过这两组常量，**默认值生效**。

### R2 — 客户端 TX 派发路径上挂着分片器：**成立**

`smoltcp/…/iface/interface/mod.rs`：`dispatch_ip(tx_token, meta, packet, fragmenter: &mut Fragmenter)`（定义 `:1066`），调用点 `:665` / `:679` / `:694` / `:742` / `:1193`。分片条件 `:1296`：

```rust
let total_ip_len = ip_repr.buffer_len();      // = header_len() + payload_len()（wire/ip.rs:757）
let should_fragment = total_ip_len > self.caps.ip_mtu();
```

两端（客户端核 `StackB` 与出口拦截栈）都用 `crate::wgcore::stackb::TunDevice`，其 `capabilities()` 写死 `Medium::Ip` + `max_transmission_unit = MTU = 1280`（`stackb.rs:31`、`:160-170`）。

### R3 — 「UDP 载荷 > 1252 即分片」：**成立，已实证**

实测（客户端栈 `StackB` → `drain_tx`）：

```
PROBE payload=1252 send_ok=true emitted=1 [len=1280 off=0 mf=false]
PROBE payload=1253 send_ok=true emitted=2 [len=1276 off=0 mf=true | len=25 off=1256 mf=false]
PROBE payload=1400 send_ok=true emitted=2 [len=1276 off=0 mf=true | len=172 off=1256 mf=false]
```

阈值推导一致：`28 + payload > 1280` ⇒ `payload ≥ 1253` 分片；分片载荷 = `max_ipv4_fragment_size(20)` = `(1280-20) - ((1280-20) % 8)` = **1256**（`phy/mod.rs:342-345` + `IPV4_FRAGMENT_PAYLOAD_ALIGNMENT = 8`）。

### R4 — **【订正·新增】客户端对 >1472 的载荷静默丢弃**：**成立**

`Fragmenter.buffer` 是**定长数组** `[u8; FRAGMENTATION_BUFFER_SIZE]`（`fragmentation.rs:277`），默认 **1500**（`build.rs:15` 的 `CONFIGS`）。`dispatch_ip` 在分片分支里：

```rust
if frag.buffer.len() < total_ip_len {
    net_debug!("Fragmentation buffer is too small, at least {} needed. Dropping");
    return Ok(());                 // ← 静默：不报错、不回执、`send_slice` 早已返回 Ok
}
```

`total_ip_len = 20 + 8 + payload` ⇒ **`payload ≥ 1473` 在客户端就被丢掉，根本不上隧道**。实测：

```
PROBE payload=1472 send_ok=true emitted=2 [len=1276 off=0 mf=true | len=244 off=1256 mf=false]
PROBE payload=1473 send_ok=true emitted=0 []      ← 静默
PROBE payload=2000 send_ok=true emitted=0 []
PROBE payload=4096 send_ok=true emitted=0 []
PROBE payload=65000 send_ok=true emitted=0 []
```

**这是第二条静默丢弃路径**，且它**在出口侧之前**——出口侧重组无论怎么做都救不了它。（设计门独立复验：机制与算术逐值吻合，采纳。）

### R5 — 出口侧现状：分片在 `on_plain` 里被直接丢弃 + 计数：**成立**

`crates/homeway-core/src/server/intercept/mod.rs:947` `pub fn on_plain`，`:955-958`：

```rust
if v.is_fragment() { self.stats.incr_frag_drop(); return; }
```

位置在 `Ipv4View::parse` 之后、`served_ports` demux（`:978`）与 `by_five` 查表（`:993`）之前——**不进会话表**。`is_fragment()` = `frag_off != 0 || mf`（`nat.rs:104-107`），`frag_off`/`mf` 读自 `pkt[6..8]`（`nat.rs:57-59`），**DF-only（0x4000）不误判**。

### R6 — F7 是相对修前的净改善：**成立**

`docs/reviews/AUDIT-2026-10-07.md:37` 记录修前形态：`nat.rs` 不读 flags/`frag_off` ⇒ 非首片被当独立会话（吃 `MAX_UDP_SESSIONS = 4096` 表）、首片被 `rewrite_dst`（`nat.rs:200`）把端口/校验和写进**分片载荷**（数据污染）。

### R7 — Go（gVisor）会重组，且重组对拦截层**完全透明**：**成立**

- gVisor 的 IPv4 端点自带重组：`gvisor/…/network/ipv4/ipv4.go:1962`
  `fragmentation.NewFragmentation(fragmentblockSize /*8*/, HighFragThreshold, LowFragThreshold, ReassembleTimeout, …)`。
  - `ReassembleTimeout = 30 * time.Second`（`ipv4.go:39-48`，注释明写「与 Linux 一致」，引 `include/net/ip.h` 的 `ipfrag_time`）。
  - `HighFragThreshold = 4 << 20`（4 MiB）／`LowFragThreshold = 3 << 20`（3 MiB）（`network/internal/fragmentation/fragmentation.go:32-42`，注释明写对齐 `ipfrag_high_thresh`/`ipfrag_low_thresh`）。
  - 越界/对齐非法 ⇒ `ErrInvalidArgs`；部分重叠 ⇒ `ErrFragmentOverlap`；冲突 ⇒ `ErrFragmentConflict`——`f.Process` 里三者都走 `f.release(r, false)` = **整条丢弃**（`fragmentation.go:200-208`）；满高水位时从队尾（最老）淘汰到低水位（`:215-225`）。
  - 超时按 `createdAt` 计（**每片到达不续期**）：`releaseReassemblersLocked` 用 `now.Sub(r.createdAt)`（`fragmentation.go:271-289`）。
- **Go 的拦截层不碰分片**：`grep -rn "frag\|Fragment\|reassembl" baseline/homeway/pkg/intercept/*.go` **零命中**。`stack.SetTransportProtocolHandler(UDP, in.handleUDPPacket)`（`intercept.go:186`）挂**传输层**入口——gVisor 的 IPv4 重组在网络层完成，交付传输层的是**已重组报文**。⇒ Go 侧「重组后走与正常包完全相同的路径」是**架构保证**，正是本批约束 1 要复刻的性质。

### R8 — TCP 不受影响：**成立**

MSS 由 MTU 推导（`nat.rs:312` 构造 SYN 带 `MSS = 1240 = 1280-40`），客户端 smoltcp 的 TCP 段恒 ≤ MSS ⇒ 永不分片。分片只出现在 UDP（`udp_send` 的 `send_slice` 接受任意长度，`wgcore/mod.rs:676-690`）。

### R9 — **【订正·被设计门推翻】反向路径现状**：**「返向通」不成立**

**v1 原写**：「≤1252 双向通；1253–1472 出向被 F7 挡、**返向通**；≥1473 双向在各自 TX 侧静默丢」。

**订正**：**1253–1472 的返向（出口→客户端）在今天就通不了**——不是被 F7 挡，而是被出口 TX 侧的反重写破坏（见 R15）。正确的现状是：

| 载荷 | 客户端→出口（出向） | 出口→客户端（返向） |
|---|---|---|
| ≤ 1252 | 通 | 通 |
| 1253–1472 | **丢**（RX：Q-B F7 的 `fragDrop`） | **静默丢**（TX：`on_tx` 破坏首片 UDP 校验和，R15） |
| ≥ 1473 | **静默丢**（客户端 TX，R4） | **静默丢**（出口 TX，同一个 1500 字节钳制，R15 邻域） |

### R10 — 客户端侧（接收）重组能力：**成立，但有并发上限**

`smoltcp` 的 `PacketAssemblerSet`（`fragmentation.rs:186-200`）槽数 = `REASSEMBLY_BUFFER_COUNT`，默认 **1**；`Interface::new` 里 `reassembly_timeout = 60s`（`interface/mod.rs:269`）。因 `std` 打开 `alloc`，`PacketAssembler` 的 buffer 是 `Buffer::new()`（Vec）且 `set_total_size` 会 `resize` 生长（`fragmentation.rs:88-97`）⇒ **单条大报文可重组，但同时只允许 1 条在途**。

### R11 — **【订正措辞】`build_icmp_unreachable` 是 Rust 侧函数，不是 Go 侧**

`grep -rn "build_icmp_unreachable" baseline/homeway` **零命中**——该函数在 `crates/homeway-core/src/server/intercept/nat.rs:267`，构造 ICMP type 3 code 3（port unreachable），载荷 = 原 IP 头 + 前 8B，`view.proto != 17 ⇒ None`，调用点 `intercept/mod.rs:1406`（halted）/`:1422`（UDP 会话满）。

Go 侧的对应能力是 **gVisor netstack 自身的 ICMP 产出**，与本批相关的：

- `gvisor/…/network/ipv4/icmp.go:816-830` `OnReassemblyTimeout(pkt)`：注释直引 RFC 792「重组超时可发 time exceeded；**若 0 号片不在则不必发**」，实现 `if pkt != nil { p.returnError(&icmpReasonReassemblyTimeout{}, pkt, true) }`。
- `icmp.go:727-728`：`icmpReasonReassemblyTimeout → header.ICMPv4TimeExceeded, header.ICMPv4ReassemblyTimeout` = **type 11 code 1**。
- `deliveredLocally=true` ⇒ 响应源地址 = 原报文目的地址（`icmp.go:649-655`）。
- 抑制集：目的为广播/组播、或**源为 `0.0.0.0`** ⇒ 不发（`icmp.go:645-647`）。
- 载荷长度：按 **RFC 1812** 发「尽可能多」（上限 `min(route.MTU, IPv4MinimumProcessableDatagramSize=576) − 8` = **548B**），非 RFC 792 的 8B 最小（`icmp.go:745-772`）。
- 限频：栈级 `NewICMPRateLimiter` 建的是 1000/s、burst 50（`stack/icmp_rate_limit.go:21-27`），但其注释明写「默认不对任何 ICMP 类型施加限制」，实际生效点在 `allowICMPReply`——`icmpRateLimitedTypes` 默认**空集** ⇒ 直接 `return true`（`ipv4.go:1847-1860`）。**结论（实际不限频）成立，v1 引的「默认 disabled」依据不准，已订正为本条。**

### R12 — **【订正·重要】本仓 Rust 客户端不会看到我们发的 ICMP 错误**

`smoltcp/…/iface/interface/ipv4.rs:320-365` `process_icmpv4`：只对 `Icmpv4Repr::EchoRequest` 回 EchoReply，`EchoReply => None`，其余 `_ => None`（仅 `socket-icmp` socket 能接）。客户端核**没有** ICMP socket（`wgcore` 只建 tcp/udp socket）⇒ **ICMP 错误被静默丢弃，不会送达 UDP 端点**。决定 §5 的价值边界。（设计门独立复验，采纳。）

### R13 — 分片不占会话表、重组必须在建会话之前：**成立**（F7 的既有性质，本批必须保住）

现状：`on_plain` 的 F7 分支在任何 `alloc_flow` 之前返回；`tcp_new`/`udp_new` 是唯一建流入口（`:1029-1031`，`const` 面 grep 已核）。

### R14 — 出口侧常量与预算（供 §3 对齐口径）

| 常量 | 值 | 位置 |
|---|---|---|
| `MAX_CONNS`（TCP） | 1024 | `intercept/mod.rs:46` |
| `MAX_UDP_SESSIONS` | 4096 | `:60` |
| `FLOW_BUF` / `FLOW_TX_BUF` | 256 KiB / 1 MiB（per TCP flow） | `:80` / `:84` |
| `WATERMARK` | 256 KiB | `:86` |
| `TX_DEFER_MAX_BYTES` | `16 × TX_SHAPE_BURST` = 4 MiB | `:401` |
| `MAX_OUT_UDP_PKTS` / `UDP_OUT_GATE` | 64 / 128 KiB | `:68` / `:72` |
| `DEFAULT_MAX_DEVICES`（出口设备表） | 32 | `server/table.rs:27` |
| per-UDP-session 栈缓冲 | rx 64 KiB + tx 64 KiB | `:2305-2306` |
| rw_port 分配域 | `[20000, 61000)` | `:1250-1264` |

⇒ 既有量级：`MAX_CONNS × 1 MiB` ≈ 1 GiB + `MAX_UDP_SESSIONS × 128 KiB` ≈ 512 MiB。本批新增上界见 §3.2/§3.3。

### R15 — **【设计门新增·高危】出口 TX 侧 `on_tx` 无反重写分片门：返向大包今天就被破坏**

`on_tx`（`intercept/mod.rs:1898-1917`）对**每一个**出口包做：

```rust
let Some(v) = Ipv4View::parse(&pkt) else { self.tx_out.push(pkt); return; };
if v.src == self.cfg.tunnel_ip && self.by_rw_port.contains_key(&v.src_port) {
    … nat::rewrite_src(&mut pkt, f.orig_dst.0, f.orig_dst.1);   // → fix_ip_checksum + fix_l4_checksum
}
self.tx_out.push(pkt);
```

`pump` 的 TX 排空对**每个包**调它（`:1556-1558`/`:1569-1571`/`:1595`/`:1606`）。⇒ 出口栈按 MTU=1280 分片后，**每个分片各自过一次 `on_tx`**，而它**没有分片门**：

1. **首片**（`off=0, MF=1`，`Ipv4View::parse` 读到真 UDP 头 ⇒ `src_port == rw_port` **必命中**）：
   smoltcp 在分片**之前**已把 UDP 头（含**整报文**长度域与**整报文**校验和）写进 `frag.buffer`（`emit_ip` 调 `packet.emit_payload` → `wire/udp.rs:286-309` 的 `emit`：`set_len(8 + inner_payload.len())` + `fill_checksum(src,dst)` 覆盖全长），首片正是从 `frag.buffer[..first_frag_ip_len]` 拷出的。
   随后 `fix_l4_checksum`（`nat.rs:159-183`）用 **`l4_len = pkt.len() - ihl`（首片长度 1256，非整报文长度）** 重算并覆盖 ⇒ **校验和被写坏**。客户端重组后 `UdpRepr::parse` 校验失败 ⇒ **静默丢**。
2. **非首片**（`off>0`）：`v.src_port` = 载荷前 2 字节（垃圾）。`by_rw_port` 命中概率 ≈ 在册流数 / 65536（rw_port 域 41000 值，但查的是 map 键 ⇒ ≈ `flows/65536`，满表 ≈ 5120 ⇒ **最高 ≈ 7.8%**）。命中后：IP 源被改成**另一条流的原始目的 IP**（`rewrite_src` 写 `pkt[12..16]`）⇒ 客户端重组键 `(src,dst,ident,proto)` **分裂** ⇒ 该报文永不完成、占客户端唯一重组槽 60s（R10）；且 `pkt[ihl..ihl+2]`（**载荷前 2 字节**）被改写（数据污染）。
3. 附带：命中时还会调 `f.obs.note_tx_seg(&v)`（`mod.rs:1902-1905`），把垃圾"载荷长度"计入 8n 观测面。

**结论**：**R9 的「返向通」不成立**；**F5-a 的「顺带修复反向路径 ≥1473 的静默丢弃」也不成立**（不加本项，只是把「发端静默丢」换成「发端发出去、收端静默丢 + 概率性数据损坏」）。⇒ 新增 **F5-d**（§2）。

---

## 2. 修复清单

### F1（核心）出口侧有界 IPv4 分片重组

**目标**：入站 IPv4 分片在**建会话之前**被有界重组；重组成功的报文**走与正常包完全相同的路径**。

**方案**

1. **新模块** `crates/homeway-core/src/server/intercept/reasm.rs`（`mod.rs` 已 5333 行；重组是有独立不变量的子系统）。`pub(crate)` 面：
   - `struct FragKey { src: Ipv4Addr, dst: Ipv4Addr, ident: u16, proto: u8 }`（**newtype 化**；字段集对齐 gVisor `FragmentID{Source,Destination,ID,Protocol}`）。
   - `struct Reassembler { ctx: HashMap<FragKey, Ctx>, bytes: usize, seq: u64 }`。
   - `enum PushOutcome { Pending, Done(Vec<u8>), Dropped(DropReason) }`（`#[non_exhaustive]`）；
     `enum DropReason { Bad { .. }, Overlap, Conflict, Limit(Evicted) }`——**统一原因通道**（`sweep` 也复用 `Evicted { reason, packets, hdr: Option<Vec<u8>> }`）。
   - `Ctx { key, hdr: Option<Vec<u8>> /* 首片 IP 头（含选项，≤60B） */, pieces: Vec<Piece>, total_len: Option<usize>, bytes: usize, created: Instant, seq: u64 }`，
     `Piece { off: u32 /* 字节偏移 */, data: Vec<u8> }`，**按 `off` 升序**。
     **无 `last` 字段**：超时按 `created` 计（对齐 gVisor `createdAt` 语义，**每片到达不续期**）——这一句写进代码注释，防实现者"顺手续期"。
2. **轻量 IP 头视图 + 一次取片**（`nat.rs` 增，**类型承担不变量**）：

   ```rust
   pub(crate) struct FragSlice<'a> { pub hdr: Ipv4FragHdr, pub payload: &'a [u8] }
   impl<'a> Ipv4FragHdr {
       /// 只解 IP 头（不碰 L4）；校验集见下，任一不满足 ⇒ None。
       pub fn parse(pkt: &'a [u8]) -> Option<FragSlice<'a>>;
   }
   ```

   - **校验集写死**（与 `Ipv4View::parse` 同口径）：`pkt.len() >= 20`；version == 4；`ihl = (pkt[0]&0x0f)*4` 且 `20 <= ihl <= 60`；`pkt.len() >= ihl`；`total_len >= ihl`；`total_len <= pkt.len()`。
   - **片载荷取段写死**：`payload = &pkt[ihl .. total_len]`（**不是** `pkt[ihl..]`——尾随字节必须丢，否则填充被夹带进重组结果）。
   - 理由：现状 `Ipv4View::parse` 对「非首片且 `body.len() < 8`」的合法小分片返回 `None`（`nat.rs:76`/`:83`），会被当畸形静默丢而非走分片路径（`fragDrop` 不涨）——**分片判定不应依赖 L4 可解析性**。
   - 调用点只传 `FragSlice`（`push(&mut self, frag: FragSlice<'_>, now: Instant)`），**签名层面杜绝 `hdr` 与 `pkt` 来自不同包**。
3. **`on_plain` 重构为两段，分片语义只有一个入口**：

   ```rust
   pub fn on_plain(&mut self, pkt: Vec<u8>) {
       match nat::Ipv4FragHdr::parse(&pkt) {
           None                  => {}                                  // 现状口径：畸形静默丢
           Some(f) if !f.hdr.is_fragment() => self.route_plain(pkt),    // 非分片：与现状逐字节同路径
           Some(f) => match self.reasm.push(f, Instant::now()) {
               PushOutcome::Pending    => {}                            // 只碰 self.reasm
               PushOutcome::Done(full) => self.route_plain(full),       // 同一入口
               PushOutcome::Dropped(r) => self.stats.note_frag_drop(r),
           },
       }
   }

   /// 原 `on_plain` 的 parse 之后全部逻辑（demux → by_five → rewrite_dst → 新建）。
   fn route_plain(&mut self, pkt: Vec<u8>) { … }
   ```

   - **F7 分支从 `route_plain` 中删除**：分片判定唯一入口 = `Ipv4FragHdr::parse` 的 `is_fragment()`；`route_plain` 里改为 `debug_assert!(!v.is_fragment())`（测试期绊线，release 无行为）。论证二者不可能不一致：两处读的是**同一对字节** `pkt[6..8]`、判定式同为 `off != 0 || mf`。
   - `route_plain` 是**唯一**投递入口 ⇒ 「分片特判旁路」在结构上不存在（满足约束 1）。
4. **组装**（完成时；逐条串行）：
   - 头 = `Ctx.hdr`（**恒存在**：完成要求覆盖 `[0, total_len)`，只有 `off == 0` 的片能覆盖起点）。
   - 输出 `Vec<u8>` 长度 = `ihl + payload_total`；逐片 `buf[ihl + off ..][..len].copy_from_slice(data)`。
   - 改头：`total_len = ihl + payload_total`；`flags/frag_off = 0`（MF/DF/off 全清）；`fix_ip_checksum`（`nat.rs:146`）重算 IP 校验和。
   - **重组器不碰 L4**：L4 校验和（含 UDP 的 0 值）随后由既有 `rewrite_dst` 按**与正常包相同**的规则处理（正常包本来也走 `fix_l4_checksum`；UDP 结果 0 时写 0xFFFF 是既有行为）。⚠️ 不要给分片路径加任何校验和特判。
   - TTL/DSCP/ECN/ident 全部保留首片值（gVisor 同形；我们不转发、不递减 TTL）。
5. **超时清扫**：`sweep(now)` 在 `pump()` 与 `pump_hold()` 的**开头**各调一次（上下文 ≤ `REASM_MAX_CTX` ⇒ 线性扫 O(64) 可忽略；`pump` 每拍 1–5ms 必到，`server/engine.rs:1114-1167`）；`close()` 里整体清空。时基 = `std::time::Instant`（**不用 smoltcp 的 `SmolInstant`**——后者在测试中由调用方驱动，30s 语义需要真实钟）。

**涉及文件**：`crates/homeway-core/src/server/intercept/reasm.rs`（新）、`intercept/mod.rs`、`intercept/nat.rs`。

**风险**

| 风险 | 处置 |
|---|---|
| 重组结果被 `Ipv4View::parse` 拒 | 输出严格 `total_len = ihl + payload`；T1/T16 断言 |
| 重组后包 > MTU 被栈 RX 拒 | **不会**：`TunDevice::receive` 不查 MTU（`stackb.rs:139-152`）；smoltcp ingress 无 `max_transmission_unit` 校验（全仓 grep 仅 TX 侧）；出口 UDP 会话 rx/tx 各 64 KiB（`:2305-2306`），`PacketBuffer::enqueue` 只要求 `capacity >= size` |
| 首片后到/缺失 | `hdr` 在 `off == 0` 到达时记录；缺失 ⇒ 永不完成 ⇒ 30s 超时回收（不发 ICMP，R11） |
| 复活「分片当会话」/「载荷污染」 | 结构保证（F1.3）+ T14 断言 `flow_count()` 恒不变 |
| 驱动线程阻塞 | 纯内存拷贝 ≤64 KiB、无系统调用、无锁；组装逐条串行 |

**判据行影响**：§7。

---

### F2 计数 / 观测面（`fragDrop` 重定义 + 6 个 additive 计数）

- `fragDrop`（**重定义**，单位 = **分片包**）：被丢弃的分片包**总数**（RX 侧）。恒等式（T26 断言）：
  **`fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`**。
- 新增（`Stats::snapshot()` **追加末位**，数组 9 → 15）：
  - `fragReasm`：成功重组并交付的**报文（datagram）数**——唯一非包单位，文档与登记表都写明。
  - `fragBad`：**非法**分片包数（偏移越界 / 非末片非 8 倍 / 空片）。
  - `fragOverlap`：因**重叠/冲突**整条丢弃所连带的**分片包数**（攻击面信号）。
  - `fragTimeout`：因**超时**淘汰所连带的**分片包数**（丢包/占用信号）。
  - `fragLimit`：因**上限**（上下文数/每源/片数/字节）被拒的**分片包数**（资源信号）。
  - `txFragDrop`：**TX 侧**（F5-d）因「无首片/TX 分片表未命中」被丢弃的分片包数（对照面：RX 的 `fragDrop`）。
  > 设计门【中-10-2】要求「可独立核对」⇒ 采纳加 `fragBad`（代价 +8B 载荷，恒等式四项全部直接可读，不需作差反推）。
- 接线（additive，旧载荷兼容）：`EngineInterceptBits`（`server/engine.rs:188-198`）追加 6 字段 + 消费点 `:891-908`；`ServeInterceptBits`（`daemon/proto.rs:350-365`）追加 6 字段（全 `#[serde(default)]` + camelCase）；`crates/homeway-cli/src/unified_cli.rs:391` 邻域同步。**DC18 人读行文不变**（`daemon_cli` 仍只渲染 4 键）——与 Q-B 同纪律。
- **限频日志（键组成与上限写死；设计门【中-1】）**：
  - 形态键 = **纯 kind**（`"frag_limit"` / `"frag_timeout"`），**不含 `src`、不含任何对端可控维度**——`src` 只进**行文**、**不进键**。遵守 `dial_fail_seen` 的既有整改纪律（`mod.rs:832-835` 注释：键含源则每条都成「首行」，降噪失效）。
  - 表 = 两个具名字段 `(u64, bool)`（键集编译期封闭，**不用 HashMap**，从结构上杜绝无界增长）。
  - 节流 = 首行 + 每 100 次一条累计行（与 `dial_fail_seen` 的 `is_multiple_of(100)` 同节奏）。
  - 行文：`intercept: 分片重组超上限（上下文 {ctx}/{max}，源 {src} {per}/{per_max}）——新报文暂不可重组（累计丢 {n} 片）` / `intercept: 分片重组超时（{n} 片，{secs}s）——整条丢弃（累计 {m}）`。
  - **重叠/冲突/非法不记行**（纯计数）——它们可被对端逐包诱发，逐条记行 = 自我放大。**有意取舍，登记**。

**涉及文件**：`intercept/mod.rs`、`server/engine.rs`、`daemon/proto.rs`、`crates/homeway-cli/src/unified_cli.rs`。

**风险**：数组 9 → 15（既有断言只用 `[0]`/`[1]`/`[2]`，追加末位不破）；status 载荷 +48 字节。

**判据行影响**：§7。

---

### F3 判据登记

本批**零编号判据行变更**（全仓 grep 已核：`fragDrop`/分片语义只出现在 `docs/INTEROP-CRITERIA.md:206`「已知口径注记」与 `:594`「计数输入集表」两处**非编号**登记；DC18/E12/E22 行文不含分片语义）。动作见 §7：已知口径注记新增 1 条 + 判据变更记录 1 行 + 计数输入集 3 行 + **登记表尾部**批注块追加 1 行。

**涉及文件**：`docs/INTEROP-CRITERIA.md`（只此一件；随代码同批 commit）。

**风险**：登记遗漏 = 静默破坏对齐 ⇒ 登记行与代码**同一 commit**（硬规则 4）。

---

### F4 ICMP Time Exceeded（裁定 = **做**；范围与分歧见 §5）

- `nat.rs` 抽出内核 `fn build_icmp(orig: &[u8], ty: u8, code: u8) -> Option<Vec<u8>>`：
  - **去掉 proto 门**（内核不判协议）——gVisor 的重组超时对任意 proto 都发（`icmp.go:816-830` 无 proto 判据）。**薄封装** `build_icmp_unreachable`（type 3 / code 3）**自行保留 `proto == 17`** 检查，维持 `on_plain` 现状不动。
  - **补抑制集**（对齐 `icmp.go:645-647`）：`orig.src == 0.0.0.0`、`orig.dst ∈ 224.0.0.0/4`、`orig.dst == 255.255.255.255` ⇒ `None`。
  - 载荷长度：**保持既有 8B 形态**（`IP 头 + 前 8 字节`）——**与 gVisor 的 RFC 1812「≤548B」不同**，登记为**有意分歧**（理由见 §5.3）。
- 触发点：`sweep` 返回的 `Evicted { hdr: Some(first_frag), .. }` ⇒ `tx_out.push(build_icmp(&first_frag, 11, 1))`；`hdr.is_none()` ⇒ **不发**（对齐 gVisor `if pkt != nil`）。
- 路径：`tx_out` → `route_encap`（`engine.rs:1358-1368`，按 `nat_view_dst` 查 peer）⇒ 只回**已认证 peer**。

**涉及文件**：`intercept/nat.rs`、`intercept/mod.rs`。

**风险**：无反射/放大（不经公网；诱发 ≥1 片、回报 ≤76B；产量结构性 ≤ 64/30s ≈ 2.1/s）。对本仓 Rust 客户端**不可见**（R12）——必须登记。

---

### F5 客户端侧与出口 TX 侧（三处，全部纳入本批）

#### F5-a 客户端发侧能力（**必修**）

根 `Cargo.toml` 的 smoltcp 依赖加 `fragmentation-buffer-size-65536`（`smoltcp/Cargo.toml` 确有该项）。

- 效果 1：客户端可发任意合法 UDP（≤ 65507 载荷）——**对齐 Go**（gVisor 无 1500 字节钳制）。
- 效果 2（**须登记**）：**同一编译单元**下**出口拦截栈的分片缓冲同步抬高** ⇒ 出口→客户端方向的大 UDP 回复**真的开始分片**（此前 ≥1473 静默丢）⇒ **wire 形态从 1 包变 N 包**。这是本批引入的**线上形态变化**，登记见 §7.3 第 3 行。
- 代价：每个 `Interface` 的 `FragmentsBuffer` 常驻 +62.5 KiB（1500 → 64 KiB）；本仓只有 2 个 stack（客户端核 `stackb.rs:196`、出口拦截 `intercept/mod.rs:851`）⇒ **+125 KiB**。
- **前置**：**必须与 F5-d 同批落地**（否则 ≥1473 的返向包从「静默丢」变成「分成 N 片后被 `on_tx` 写坏校验和 → 收端静默丢 + 概率性污染」，性质更差）。

#### F5-b 客户端发侧可见失败门（**必修**）

`ClientCore::udp_send`（`wgcore/mod.rs:676`）前置门：`20 + 8 + data.len() > 65535` ⇒ `Err(ConnErr::DatagramTooLarge)`（`wgcore` 错误 enum 追加变体，thiserror，**不用字符串错误**）。区间 (1253, 65507] **不报错**（那时是正常可发）。⇒ 消掉「发了但出不去」的最后一段静默。

#### F5-c 客户端收侧并发槽位（**建议做**）

加 `reassembly-buffer-count-8`（R10：默认 1 ⇒ 同时只能重组 1 条入站大报文）；alloc 下空槽几乎零堆占用。**不做的后果**：客户端同时收到两条大 UDP 回复时丢一条（UDP 语义内的丢包，非静默）。裁定：纳入；若代码门判定风险偏高可降级为「登记残余」。

#### F5-d 出口 TX 侧反重写的分片感知（**必修，由设计门【高-1】带出；见 R15**）

**现状缺陷**：`on_tx`（`intercept/mod.rs:1898-1917`）无分片门 ⇒ 分片回复的**首片** UDP 校验和被 `fix_l4_checksum` 按**首片长度**重算覆盖（错），**非首片**有 ≈`flows/65536` 概率撞 `by_rw_port` 被误改 IP 源与载荷前 2 字节。

**方案**

1. **新增 TX 侧极轻量 IP 头视图**（复用 F1.2 的 `Ipv4FragHdr::parse`）。
2. `on_tx` 三分支：
   - **非分片**（`off == 0 && !mf`）⇒ **现状逐字节不变**（by_rw_port 查找 + `rewrite_src` 全量重算）。
   - **首片**（`off == 0 && mf`）⇒ 现行 by_rw_port 查找；命中则改写 IP 源 + 端口，**并用 RFC 1624 增量更新 L4 校验和**（见下）；随后**登记 TX 分片表**（见 3）。未命中（真 listener 应答，如 DNS 面）⇒ 不改写，也登记（值 = 不重写）。
   - **非首片**（`off > 0`）⇒ **只改 IP 源地址 + IP 头校验和**（L4 头不在此片，**不碰端口、不碰 L4 校验和**）；源地址取值查 TX 分片表。
     - 表中 `Some(orig)` ⇒ 改成同一 `orig`（**同报文所有分片必须携带同一个改写后 IP 源**，否则客户端重组键分裂）。
     - 表中 `None` ⇒ 什么都不做（该报文本就不需重写）。
     - **表未命中** ⇒ 首片丢失/已过期 ⇒ **丢弃该片 + 计数**（`txFragDrop`）。理由：首片既丢，报文本就无法重组；继续发出只会占客户端唯一的重组槽 60s（R10）。**丢片比发坏片干净**（且可观测）。
3. **TX 分片表**（`tx_frag: HashMap<TxFragKey, TxFragVal>`，`TxFragKey = (dst: Ipv4Addr, ident: u16, proto: u8)`，`TxFragVal = { orig: Option<(Ipv4Addr, u16)>, created: Instant }`）：
   - **键的完备性**：`dst` = 客户端隧道 IP（每 peer 唯一），`ident` 由 smoltcp 的 `next_ipv4_frag_ident()` 按 Interface 递增 ⇒ 一个在途报文内唯一。
   - **回收**：处理到该键的**末片**（`mf == false`）时**立即删除**（精确回收）；另设 `TX_FRAG_TTL = 10s` 与 `TX_FRAG_MAX = 64` 兜底（丢片/异常时兜底，淘汰最老）。上位界 = 64 × (≈40B) ≈ 2.5 KiB。
   - 注：smoltcp 的 `Fragmenter` 是**单缓冲**（一个 Interface 同时只有 1 条在途分片报文，剩余片在后续 poll 续发，`interface/mod.rs:597-598`）⇒ 正常态表内 ≤1 项。
4. **RFC 1624 增量更新**（只用于首片；拿不到整报文载荷，**不能**用 `fix_l4_checksum` 的全量重算）：
   - 改动字 = 伪头源地址 2 个 16 位字（`tunnel_ip` → `orig_dst_ip`）+ UDP 源端口 1 个 16 位字（`rw_port` → `orig_port`）。
   - `HC' = ~(HC + ~m + m')`，逐字更新；结果若为 `0x0000` 则写 `0xFFFF`（UDP 规则）。
   - **UDP 校验和原值为 0** ⇒ 保持 0（RFC 768「无校验」不可被 NAT 变成非 0）。
   - 仅对 `proto == 17` 生效；TCP 永不触发（MSS 由 MTU 推导，R8）。
5. `f.obs.note_tx_seg(&v)` 只在首片/非分片命中时调用（修掉 R15-3 的观测面污染）。

**涉及文件**：`intercept/mod.rs`（`on_tx` 重构 + 表 + 字段）、`intercept/nat.rs`（RFC 1624 增量更新纯函数）、`intercept/reasm.rs`（`Ipv4FragHdr` 复用）。

**风险**

| 风险 | 处置 |
|---|---|
| 增量更新写错 ⇒ 客户端全线 UDP 校验失败 | 单测**直接取 smoltcp 产出的真分片**做前后对照（T25：整报文校验和 == 增量更新后的值），并做端到端 T24 |
| 非首片源地址与首片不一致 ⇒ 客户端重组永不完成 | 表键覆盖同一报文的全部片；无表项直接丢片（不发坏片）；T26 断言「同报文全部分片改写后 IP 源一致」 |
| 表无界增长 | 精确回收（末片删）+ TTL + 计数上限（上位界 2.5 KiB） |
| 与 F5-a 的耦合 | **同批落地**；T24/T25 在 F5-a 打开的前提下跑 |

**判据行影响**：§7.3 第 3 行（线上形态变化）+ §7.4。

---

### F6（不做项，登记理由）客户端入站重组超时（smoltcp 60 s）不改

客户端侧 `reassembly_timeout = 60s`（R10）比出口侧 30s 宽。**不改**：属客户端核内部实现（不属隧道线协议/契约面），且只在「分片丢失」时耗资源——客户端槽位少，改它收益低、引入新特性面无必要。

---

## 3. 安全面评估

### 3.1 攻击面逐条

| # | 攻击面 | 处置 | 残余 |
|---|---|---|---|
| A1 | **teardrop / 重叠** | 部分重叠（越出 hole 边界）⇒ 整条丢弃；同区间不同字节 ⇒ 冲突 ⇒ 整条丢弃（§4） | 无（比 gVisor 更严，见 §4.1/§4.3，已登记） |
| A2 | **资源耗尽（上下文数）** | 全局 `REASM_MAX_CTX = 64`；超限**淘汰最老**（gVisor `fragmentation.go:215-225` 同形），不拒新 | 攻击者可持满 64 槽 ⇒ 合法大 UDP 重组失败（降级为丢包；**TCP / 小 UDP 不受影响**）；已登记 |
| A3 | **单源占满** | 每源 `REASM_MAX_PER_SRC = 4`（**公平性闸**：合法单设备实际恒 ≤1，抗多源伪造靠全局 64） | 伪造多源可绕过每源闸，被 A2 钳住；威胁模型 = 已认证隧道设备（与 Q-E 同口径） |
| A4 | **内存耗尽（单片/长片）** | 每上下文 `REASM_MAX_FRAGS = 64` 片、字节 ≤ 65535（u16 结构性）、全局 `REASM_MAX_BYTES = 4 MiB` | 无 |
| A5 | **超时窗口** | `REASM_TIMEOUT = 30s`（gVisor `ReassembleTimeout` = Linux `ipfrag_time`），按 `created` 计不续期；`pump` 每拍（1–5ms）清扫 | 最坏占用 = 30s × 64 上下文 |
| A6 | **非 8 倍 / 越界 / 空片** | 三者 ⇒ 整条丢弃（§4.2） | 无 |
| A7 | **与 `MAX_CONNS`/`MAX_UDP_SESSIONS` 交互** | 重组在建流之前 ⇒ **每个重组报文至多建 1 个会话**，与不分片**等势**；分片自身不建会话（结构保证） | 无（无放大） |
| A8 | **CPU** | 片插入 = **有序二分定位**（O(log 64)）+ 只与相邻片做区间比较 ⇒ 每片 O(log n)+O(1)；恒等式成本与隧道片速率同阶（隧道带宽受限；MTU 1280 下片速率与包速率同量级），远低于既有每包 parse/demux 成本 | 无 |
| A9 | **日志灌爆** | 仅**超限/超时**记行；键 = **纯 kind（不含 src）**、两个封闭具名字段（无 HashMap 无界增长）、节流 = 首行 + 每 100 次累计行；重叠/冲突/非法**不记行** | 有界 1 行/100 事件；**有意取舍已登记** |
| A10 | **ICMP 放大** | 只回**已认证 peer**（走隧道，不经公网）；最小诱发形态 = 首片 28B ⇒ 回报 56B（**单次 ≤2×**；代码门 r29 低-5 订正：v2 原文「放大比 < 1」按 76B 上限写错），产量结构性 ≤ 64 上下文/30s ≈ 2.1 报文/s ≈ 120B/s | 无 |
| A11 | **TX 分片表**（F5-d 新增面） | 键 `(dst, ident, proto)`；末片精确回收 + `TX_FRAG_TTL = 10s` + `TX_FRAG_MAX = 64`（淘汰最老）；正常态 ≤1 项 | 上位界 2.5 KiB |

### 3.2 每项上限的取值与依据

| 常量 | 取值 | 依据 |
|---|---|---|
| `REASM_MAX_CTX` | **64** | ≥ 2× `DEFAULT_MAX_DEVICES = 32`（客户端 smoltcp 的 `Fragmenter` 是**单缓冲** ⇒ 每接口同时只有 1 条在途分片报文）；且 64 × 64 KiB = 4 MiB = gVisor/Linux `ipfrag_high_thresh`，两套口径同时对齐 |
| `REASM_MAX_PER_SRC` | **4** | **公平性闸**：合法单设备实际恒 ≤1（单缓冲），留 4 给「连续两条报文在网中交错」的窗口；**不承担**抗多源伪造职责（那是全局闸） |
| `REASM_MAX_FRAGS` | **64** | **前提假设**：隧道契约 MTU = 1280（两端都是本仓/基线代码：`stackb.rs:31`、`baseline/homeway/internal/server/serve.go` 同为 1280）⇒ 合法最坏 = `ceil(65515 / 1256) = 53`，留 ~20% 裕度。**异种 MTU 对端**（如 MTU 576 的合法发送方 ⇒ 123 片）会被 `fragLimit` 拒——**这是有意更严 + 前提假设**，必须连同「`fragLimit` 行文带片数维度（可观测）」一起登记 |
| `REASM_MAX_BYTES` | **4 MiB** | = gVisor `HighFragThreshold` / Linux `ipfrag_high_thresh`。**量化说明**：`64 × 65535 = 4,194,240` 比 `4 MiB = 4,194,304` 只小 **64 字节** ⇒ 字节闸是**冗余第二道闸**（代码门 r29 低-3 订正：v2 原文「只在 63 条近满上下文 + 新片这一窄窗口独立触发」不成立——任何合法写入后 `Σbytes ≤ 64 × 65535 = 4,194,240 < 4 MiB`，**该分支按既有不变量不可达**，实现保留为纯防御面）。加**编译期断言**（对齐既有 `const _: () = assert!(UDP_OUT_GATE < WATERMARK);` 的形态）：`const _: () = assert!(REASM_MAX_BYTES >= REASM_MAX_CTX * 65535);` |
| 单上下文字节 | **≤ 65535** | IPv4 `total_len` 是 u16；无重叠 ⇒ Σ片长 ≤ 覆盖字节 ≤ 65535（结构性） |
| `REASM_TIMEOUT` | **30 s** | = gVisor `ReassembleTimeout`（`ipv4.go:39-48`，注释明写对齐 Linux `ipfrag_time`）；**按 `created` 计、不续期**（gVisor `createdAt` 语义，`fragmentation.go:271-289`） |
| 输出报文上限 | **65535** | u16 |
| `TX_FRAG_MAX` / `TX_FRAG_TTL` | **64 / 10 s** | smoltcp 单缓冲 ⇒ 正常态 ≤1 项；TTL 给「跨 poll 续发分片」足够余量（续发间隔 1–5ms） |
| 配置面 | **全部编译期常量** | **不新增配置键、不进 `serve.status`**（与 `MAX_CONNS`/`MAX_UDP_SESSIONS`/`TX_DEFER_MAX_BYTES` 同口径，`intercept/mod.rs:46/60/401`） |

### 3.3 最坏 RSS 影响

| 项 | 计算 | 上界 |
|---|---|---|
| 重组缓冲（数据面） | 64 上下文 × ≤65535 B | **4 MiB** |
| 组装的瞬时输出 | 逐条串行 ⇒ Σ(全部上下文) + 单条输出 | **4 MiB + 64 KiB** |
| 上下文/片元数据 | 64 × (≈120 B 结构 + 64 × ≈40 B `Piece`) ≈ 64 × 2.7 KiB | ≈ **0.2 MiB** |
| TX 分片表（F5-d） | 64 × ≈40 B | ≈ **2.5 KiB** |
| 客户端/出口 `FragmentsBuffer`（F5-a） | 2 个 Interface × (64 KiB − 1.5 KiB) | ≈ **+0.12 MiB** |
| **合计** | | **≈ 4.3 MiB** |
| **对照** | `TX_DEFER_MAX_BYTES` = 4 MiB；`MAX_CONNS × FLOW_TX_BUF` ≈ 1 GiB；`MAX_UDP_SESSIONS × 128 KiB` ≈ 512 MiB | **≈ 0.4%** |

### 3.4 与 Go / Linux 对照

| 项 | Go（gVisor） | Linux | 本设计 | 差异 |
|---|---|---|---|---|
| 超时 | 30 s（`createdAt` 起算，不续期） | `ipfrag_time` 30 s | 30 s，`created` 起算 | 无 |
| 内存高/低水位 | 4 MiB / 3 MiB（按字节，队尾淘汰到低水位） | 4 MiB / 3 MiB | 4 MiB 硬闸 + 64 上下文（淘汰最老） | 等效；**无低水位滞回**（逐条淘汰）——已登记 |
| 每源配额 | **无** | 无 | 4 | **更严**（加固） |
| 片数上限 | 无 | 无（受内存阈值） | 64（前提 MTU=1280） | **更严 + 前提假设**（异种 MTU 对端会被拒）——已登记 |
| 部分重叠（逆出 hole 边界） | 整条丢弃（`ErrFragmentOverlap`） | 整条丢弃 | 整条丢弃 | 无 |
| **内含重叠**（片落在已填充区间内、区间不同） | **静默忽略**（`reassembler.go:113-116` 的越过判定不命中 ⇒ `currentHole.filled ⇒ continue`，`reassembler.go:127-130`） | 丢弃整条 | **整条丢弃** | **比 Go 严**（与 Linux 一致）——已登记 |
| 完全相同区间重复（同数据） | 静默接受（不比较数据） | 丢弃整条 | 静默接受 | 与 Go 一致 |
| 完全相同区间重复（不同数据） | 静默接受**先到的片** | 丢弃整条 | 丢弃整条 | **比 Go 严、与 Linux 一致**——已登记 |
| 非 8 倍 / 越界 | 整条丢弃（`ErrInvalidArgs` / `ErrFragmentConflict`） | 丢弃 | 整条丢弃 | 无 |
| 超时 ICMP | type 11 code 1（仅首片在位；源 = 原目的） | `ip_expire()` 发 type 11 code 1 | type 11 code 1（仅首片在位；源 = 原目的） | 无 |
| ICMP 载荷长度 | RFC 1812：`min(全长, 548B)` | 同 RFC 1812 | **8B**（沿用既有 `build_icmp_unreachable` 形态） | **有意分歧**（§5.3） |
| ICMP 限频 | 有效不限（`icmpRateLimitedTypes` 空集，`ipv4.go:1847-1860`） | 有 sysctl 限频 | 不限，但产量结构性 ≤2.1/s | 更保守 |

**结论**：主口径（超时 / 内存量级 / 非法 / 部分重叠 / ICMP 类型码与触发条件）与 Go 逐项对齐；三处**有意更严或分歧**（每源配额、片数上限、内含重叠与冲突重复片的处置、ICMP 载荷长度）都只在**恶意 / 畸形 / 异种 MTU** 输入上产生可观测差异，合法流量上不可区分——全部登记（§7）。

---

## 4. 重叠分片与偏移合法性的取证裁定

### 4.1 重叠 ⇒ **整条丢弃**（裁定）

**取证**

- RFC 5722 §4：IPv6「重叠 ⇒ 整个数据报及其所有分片 MUST 被静默丢弃」（明文限定 IPv6；IPv4 侧无同等 MUST）。
- 两个工程实现都把「**逆出已收区间边界**的部分重叠」判为整条丢弃，且 gVisor 注释直引 Linux 作为理由：
  > *"It is not explicitly forbidden for IPv4, but to keep parity with Linux we disallow it as well: https://github.com/torvalds/linux/blob/38525c6/net/ipv4/inet_fragment.c#L349"*
  > —— `gvisor/…/internal/fragmentation/reassembler.go:84-93`
- gVisor 的实现细节（**v1 的对照在这里不准，已订正**）：`reassembler.go` 只在 `first < currentHole.first || currentHole.last < last`（片**逆出**当前空洞）时返回 `ErrFragmentOverlap`；若片**完全落在**已填充空洞内（含严格子区间），走 `currentHole.filled ⇒ continue` ⇒ **静默忽略**（`:127-130`）。`ErrFragmentOverlap/ErrFragmentConflict/ErrInvalidArgs` 三者都由 `fragmentation.Process` 走 `f.release(r, false)` = **整条丢弃**（`fragmentation.go:200-208`）。

**裁定：一律整条丢弃（含 gVisor 会静默忽略的「内含重叠」子形态）。** 理由三条：① teardrop 类解析分歧的现代实践（Linux）就是整条丢弃；② 成本不对称——「先到片胜」需要实现 hole 语义与歧义仲裁，收益为零（合法发送方永不产生重叠）；③ 差异只在**畸形/恶意**输入上可见，纯加固。

**登记**：与 Go 的这处分歧**必须登记为「有意更严」**（§7.4），否则验收方会误以为「重叠行为与 Go 同串」。

### 4.2 偏移合法性（裁定）

RFC 791 §3.1 规定**非末片的数据长度必须是 8 字节的整数倍**。裁定五条，**任一违反 ⇒ 丢弃该片并清掉整条上下文**（与 gVisor `ErrInvalidArgs ⇒ release(r)` 同形）：

1. `more == true && payload_len % 8 != 0` ⇒ 非法。
2. `off × 8 + payload_len > 65535` ⇒ 越界。
3. `payload_len == 0 && more == true` ⇒ 空片（无进展，纯占位）。
4. **`payload_len == 0 && more == false` ⇒ 同样整条丢弃**（gVisor 对任意空片都走 `first > last ⇒ ErrInvalidArgs`，`fragmentation.go:160-162`）。**不得**把空末片当「末片定位器」采纳（`total_len = off*8`）——那与 Go 不一致，且会让上下文白占 30s。
5. 若末片先到并定下 `total_len`，则 `total_len` **不可变**：其后任何「越过末尾」的片、
   或**另一片范围不同的末片**（含落在空洞里的「更短末片」）⇒ 冲突 ⇒ 整条丢弃
   （gVisor `ErrFragmentConflict`，`reassembler.go:96-101`/`:163-166`；后者为代码门 r29
   低-1 补入的子形态——v2 只写了「越过末尾」，会让更短末片**改小** `total_len` ⇒
   已收齐的报文永不完成、白占 30s）。
6. **输出长度门**：`ihl + payload > 65535`（首片带 IP 选项时 `ihl > 20`）⇒ 整条丢弃
   （代码门 r29 中-1；v2 的输出上限 65535 是登记口径但未落成闸——修前 `as u16` 静默
   回绕，产物被 `route_plain` 的 parse 拒而静默丢却已计成功）。

**另注**：`off == 0 && mf == false` 的包**不是分片**（`is_fragment()` 为假），走 `Frag::Whole` 原路径——DF-only 不误判的性质（F7）延伸保留。

### 4.3 重复片的细分裁定（**有意比 Go 严一档**）

| 子形态 | gVisor | 本设计 | 说明 |
|---|---|---|---|
| 同区间 + **同字节** | 静默忽略（不比数据） | **静默忽略**（不计数） | 与 Go 一致；合法流量不可区分 |
| 同区间 + **不同字节** | 静默采纳**先到的片** | **冲突 ⇒ 整条丢弃** | 比 Go 严（与 Linux 一致）：防同一区间两种数据的**歧义注入**（IDS 规避手法） |
| **内含但区间不同**（落在已填充区间内） | **静默忽略** | **整条丢弃** | 比 Go 严（与 Linux 一致）——v1 漏登记的子形态 |

字节比较只在「区间完全相同」时发生（成本 ≤ 片长），合法发送方不会产生该形态。

---

## 5. ICMP 裁定（**做**，但范围受限）

### 5.1 裁定

**做**：出口在**重组超时淘汰**且**首片在位**时，回 **ICMP type 11 code 1（Time Exceeded / Fragment Reassembly Time Exceeded）**，载荷 = 首片 IP 头 + 前 8 字节，源 = 首片目的地址、目的 = 首片源地址，走既有 `tx_out` → `route_encap` → 隧道路径。**任意 proto**（不设 proto 门）；抑制 `src == 0.0.0.0` 与组播/广播目的。

**不做**：`type 3 code 4`（Fragmentation Needed）与 `type 3 code 3`（Port Unreachable）在本批**不新增**（code 3 既有，仅 UDP，不动）。

### 5.2 理由（做 type 11 code 1）

1. **出口侧可观测性**（**首要理由**）：标准工具（tcpdump/出口日志）可判定「这条 UDP 是重组失败，不是网络黑洞」。这是本批唯一**现役**收益。
2. **与 Go 行为保持历史同形（无现役消费方）**：gVisor 有这条（`icmp.go:816-830` → `:727-728`），条件同形（`if pkt != nil` = 首片在位）、源/目同形（`deliveredLocally=true` ⇒ 源 = 原目的）。但 AGENTS.md 记载 **Go 版 2026-10-05 整体退役**，本仓 Rust 客户端又看不到它（R12）⇒ 它**不是**「对齐现役实现」，只是「对齐历史行为」。
3. **RFC 792 明文许可**：「若重组超时，丢弃数据报，**可以**发 time exceeded；**若 0 号片不可用则不必发」。
4. **成本极低**：`nat.rs:267` 的 `build_icmp_unreachable` 已是同一套构造（反转 src/dst + 原 IP 头前 8B + 双校验和），参数化 `type`/`code` 即可。
5. **无攻击面**：只回已认证隧道 peer（不经公网）；产量被 `REASM_MAX_CTX`(64) × `REASM_TIMEOUT`(30s) 结构性钳住（≤2.1/s）——比 gVisor 的实际行为（不限频）更保守。

### 5.3 分歧与不做项的理由

- **不做 type 3 code 4**：`Fragmentation Needed` 是**转发/出站**语义（DF 置位 + 下一跳 MTU 不够时由转发方产生）。本层是**重组终点**（把包交本机传输层），不转发；出口的出站是 OS socket，由内核 PMTU 处理。⇒ 无此形态。
- **载荷长度保持 8B（≠ gVisor 的 RFC 1812 ≤548B）**：**有意分歧**，理由 =（a）现役无消费方（Go 退役 + R12）；（b）沿用既有 `build_icmp_unreachable` 的形态与测试，避免同一文件内两套长度语义；（c）8B 满足 RFC 792 的最小要求。**登记**（§7.4），并注明「若将来需要与 Go 逐字节对齐，改动点 = 给 `build_icmp` 加一个 `max_payload` 参数」。

### 5.4 必须登记的负面事实

**R12：本仓 Rust 客户端看不到这条 ICMP**（smoltcp 的 `process_icmpv4` 只处理 Echo，其余 `_ => None`；客户端核无 `socket-icmp`）。⇒ 这条 ICMP 的价值是「出口侧可观测 + 与历史 Go 行为同形」，**不是**「修好本仓客户端的可见性」。写进登记表，否则会被误读为「客户端大 UDP 失败现在可观测了」。

---

## 6. 客户端侧 / 出口 TX 侧是否也该有动作（裁定 = **是，F5 四项全做**）

### 6.1 关键事实

1. 出口侧 RX 重组**只能**覆盖 `1253 ≤ payload ≤ 1472`（出向）。`payload ≥ 1473` 在**客户端 TX 侧**就被 smoltcp 的 1500 字节分片缓冲静默丢弃（R4，已实证）。
2. **更关键（设计门【高-1】带出，R15）**：返向（出口→客户端）的 `1253–1472` 大回复**今天也通不了**——出口 TX 侧的 `on_tx` 反重写无分片门，会把首片的 UDP 校验和按**首片长度**重算覆盖（错）⇒ 客户端重组后校验失败静默丢；非首片还有 ≈`flows/65536` 概率被误改 IP 源与载荷。

⇒ 只做出向重组，得到的是一条**半通**的能力（大请求可以送达，但大回复回不来），且与「对齐 Go 能力」的目标不符。

### 6.2 为什么不是「让客户端失败得可见」二选一

「让超大 UDP 在客户端就失败得可见」是**次优解**：它把能力缺口翻译成明确错误，但仍**不提供 Go 的能力**（Go 能发）。裁定：**两者都做**——F5-a 恢复能力（对齐 Go），F5-b 只对**结构性不可发**的 `> 65507` 报错。

### 6.3 与 Go 的分工对照

Go 是**两侧都做**：客户端 gVisor 自己分片（无小缓冲钳制）+ 出口 gVisor 自己重组 + 出口无 NAT 反重写（gVisor 用「伪装端点」直接以原目的地址发包，**不存在** `on_tx` 这种事后重写）。我们的形态多了第三处（F5-d），因为我们的出口用「栈内 socket + 事后反重写」替代了 gVisor 的 spoof 端点——**这是形态差异带来的额外必修项，不是对齐项**。

---

## 7. 判据行影响清单

### 7.1 编号判据行（E*/C*/R* / DC18 / E12 / 人读行）

**零变更。** 依据：全仓 grep 确认没有任何编号判据行的行文含分片语义；`fragDrop` 只出现在 `:206`（已知口径注记）与 `:594`（计数输入集表）两处**非编号**登记。DC18 人读行、E12 UDP 会话行、E22 DNS 行的**行文与数值语义均不动**。

### 7.2 §「判据变更记录」登记表（新增 1 行）

| 日期 | 条目 | 从 → 到 | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-K 批落地） | **IPv4 分片处置**（Q-B F7 的「接受的差异」条目） | 「拦截层丢弃入站 IPv4 分片（非首片 `frag_off>0` 或 MF 置位）并计 `fragDrop`——**拒绝 Go/gVisor 会重组的形态**；出口侧分片重组未实现」 → 「① 拦截层在建会话之前对入站 IPv4 分片做**有界重组**（30s 超时〔不续期〕/ 64 上下文 / 每源 4 / 64 片 / 4 MiB / ≤65535 字节），重组成功的报文走**与正常包完全相同**的 `route_plain` 路径；② 部分重叠、**内含重叠**、同区间不同字节、越界、非 8 倍、空片 ⇒ **整条丢弃**；③ 超时且首片在位 ⇒ 回 ICMP type 11 code 1（**载荷 8B，与 gVisor 的 RFC 1812 ≤548B 有意不同**）；④ **出口 TX 侧反重写新增分片感知**（非首片只改 IP 源；首片用 RFC 1624 增量更新 L4 校验和）」 | Q-K：恢复 Go/gVisor 的重组能力（对齐），同时保持 F7 修掉的「不占会话表 / 不污染载荷」性质；并修复设计门查出的对称缺陷（TX 侧 `on_tx` 无分片门） | `crates/homeway-core/src/server/intercept/{mod.rs,reasm.rs,nat.rs}`；`docs/INTEROP-CRITERIA.md` §已知口径注记的 Q-B F7 条目（追加收口指针）；Q-B 的 `docs/reviews/QB.md`/`QB-design.md` 不追改（历史记录） |

### 7.3 §「计数输入集 / 数值语义变化」表（新增 3 行）

| 日期 | 条目 | 从 → 到（数值语义） | 原因 | 影响面 |
|---|---|---|---|---|
| 2026-10-08（Q-K） | **`fragDrop`** | 「入站分片被**整包丢弃**的片数（唯一来源 = F7 的 `return`）」→「被丢弃的分片包**总数**：非法（越界/非 8 倍/空片）+ 重叠冲突连带 + 超时连带 + 超限拒绝」；恒等式 **`fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`**（**四项全部可独立读**）；单位仍是**包**（向下兼容） | Q-K F1/F2：分片不再一律丢，`fragDrop` 需覆盖重组的各条失败路径 | `serve.status` intercept 段、`EngineInterceptBits`、`homeway-cli` 取值点；在「已能走通」的流量上数值**降为 0**（此前该流量根本走不通） |
| 2026-10-08（Q-K） | **新增计数** `fragReasm` / `fragBad` / `fragOverlap` / `fragTimeout` / `fragLimit` / `txFragDrop`（`Stats::snapshot()` **追加末位**，数组 9 → 15） | 无 → 有 | Q-K F1/F2/F5-d：重组的成功 / 畸形 / 攻击 / 丢包 / 资源五类信号 + TX 侧无首片丢片，全部可观测 | additive：① `snapshot()` 索引 `[0..=8]` 与键查找语义不变；② 经 `serve.status` 载荷 `ServeInterceptBits` 暴露（6 键，serde `default` 兼容旧载荷）；**DC18 人读行文不变**；③ **单位差异**：`fragReasm` = **报文数**（datagram），其余五者 = **分片包数**；④ `fragDrop` 是 **RX** 侧总数，`txFragDrop` 是 **TX** 侧总数（两面独立、不可混加） |
| 2026-10-08（Q-K） | **出口→客户端方向的线上包形态** + `tx_deferred`/`shape_drop` 的输入集 | ① 出口栈对大 UDP 回复：1253–1472 载荷 ⇒ 分成 2 片发出，但**返向实际不可用**（首片 UDP 校验和被 `on_tx` 写坏 ⇒ 客户端静默丢）；≥1473 载荷 ⇒ 出口 TX **静默丢弃**。② **`on_tx` 分片感知 + `fragmentation-buffer-size-65536`** 之后：1253–1472 正常发 2 片、**≥1473 现在真的分片发出**（1 报文变 N 包）。③ 按包计数的面（`tx_win_released` 等）随之变化：1 报文 ≠ 1 包 | Q-K F5-a + F5-d：恢复返向大 UDP（对齐 Go），并修复设计门查出的校验和破坏缺陷 | 出口整形面（`TX_DEFER_MAX_BYTES` 按字节，口径不变；`tx_win_released` 按包，数值变大）；**客户端入站重组并发 = 1**（`REASSEMBLY_BUFFER_COUNT`，F5-c 后为 8）⇒ 并发大回复仍可能丢；新增计数 `txFragDrop`（F5-d 的无首片丢片） |

### 7.4 §「已知口径注记」新增 1 条

Q-K 条目，内容 = 7.2 的「到」列 + §5.4 的 ICMP 客户端不可见事实 + §4.3 的三类重复片处置（含**内含重叠**与**同区间不同字节**两处「有意比 Go 严」）+ §3.2 的「片数上限的 MTU=1280 前提假设（异种 MTU 对端会被拒）」+ §5.3 的 ICMP 载荷长度分歧 + §3.4 的「无低水位滞回」+ §6.4/F6 的残余（客户端入站 60s 超时、并发槽位）。

### 7.5 additive 观测行（非判据行，登记留痕）

新增限频日志 2 条（§2-F2）：`intercept: 分片重组超上限（…）`、`intercept: 分片重组超时（…）`；键 = 纯 kind、无 HashMap、节流 1/100。**不新增**编号行。

---

## 8. 测试计划（汇总）

### 8.1 出口侧 RX（`reasm.rs` 单测 + `intercept/mod.rs` 的 `mod tests`；复用既有 `cross_pump` 交叉泵与 `nat::build_udp`/`build_tcp_syn` 构造器）

| # | 用例 | 断言要点 |
|---|---|---|
| T1 | `reasm_two_fragments_end_to_end` | 客户端 `StackB` UDP 发 1300B（真分片）→ 出口重组 → 回环 echo 收到 1300B 且**逐字节一致**；`fragReasm == 1`、`fragDrop == 0` |
| T2 | `reasm_out_of_order` | 末片先到、首片后到 ⇒ 仍完成；`fragReasm == 1` |
| T3 | `reasm_duplicate_identical_ignored` | 同区间同数据重复片 ⇒ 计数不变、最终完成 |
| T4 | `reasm_duplicate_conflicting_drops_whole` | 同区间不同数据 ⇒ 整条丢弃；`fragOverlap == 1`；**不建会话** |
| T5 | `reasm_partial_overlap_drops_whole` | 部分重叠（逆出边界）⇒ `fragOverlap == 1` |
| T6 | `reasm_contained_overlap_drops_whole` | **内含重叠**（片落在已填充区间内、区间更小）⇒ 整条丢弃（比 Go 严的登记项） |
| T7 | `reasm_conflicting_final_drops_whole` | 两个不同 `off+len` 的末片 ⇒ 冲突丢弃 |
| T8 | `reasm_bad_offset_not_multiple_of_8` | 非末片 9 字节 ⇒ `fragBad` 增、`fragDrop` 增 |
| T9 | `reasm_zero_len_fragment_dropped` | 空片（`more` 真**与**假两态）⇒ `fragBad` 增（§4.2 第 3/4 条） |
| T10 | `reasm_oversize_offset_dropped` | `off*8 + len > 65535` ⇒ `fragBad` 增 |
| T11 | `frag_header_parse_malformed_inputs` | 截断 / `total_len > pkt.len()` / `ihl < 20` / `total_len < ihl` ⇒ `None`（静默丢，不进重组器） |
| T12 | `frag_slice_payload_excludes_trailing_bytes` | 在 `total_len` 之后追加垃圾字节 ⇒ 重组结果**不含**它们（片载荷取段钉死） |
| T13 | `reasm_timeout_evicted` | 注入 now + 31s ⇒ 上下文清空、`fragTimeout` 增、**槽位回收**（随后合法重组成功）；**另一条**：多片到达不延长寿命（不续期） |
| T14 | `reasm_ctx_limit_evicts_oldest` | 造 `REASM_MAX_CTX + 1` 条 ⇒ 最老被淘汰、`fragLimit` 增、新报文能进 |
| T15 | `reasm_per_src_limit` | 单源第 5 条 ⇒ `fragLimit` 增、该条不建上下文 |
| T16 | `reasm_frag_count_limit` | 单上下文第 65 片 ⇒ 整条丢弃、`fragLimit` 增 |
| T17 | `reasm_byte_limit` | **构造**：63 条各 ~65535B 上下文 + 1 条新 → 触发全局字节闸（或按编译期断言 `REASM_MAX_BYTES >= REASM_MAX_CTX * 65535` 判定为冗余闸） |
| T18 | `fragments_never_touch_flow_table` | 全部失败路径上 `flow_count()` **恒不变**（F7 性质保持） |
| T19 | `reassembled_takes_rewrite_dst_path` | 重组到**过境**目的（非豁免）⇒ 走 `route_upstream` 拨号、上游收到**原目的端口**（证明无旁路、`rewrite_dst` 生效） |
| T20 | `non_fragment_path_unchanged` | DF-only / 普通包 / 畸形包行为与现状逐项一致（回归） |
| T21 | `reasm_tcp_fragments_deliver` | TCP 分片（SYN 分两片）⇒ 重组后照常建流（对齐 Go 的协议无关重组） |
| T22 | `icmp_time_exceeded_on_timeout` | 超时且首片在位 ⇒ `tx_out` 出 ICMP：type 11 code 1、载荷 = 首片头 + 8B、src/dst 反转、双校验和自洽 |
| T23 | `icmp_not_sent_without_first_fragment` | 无首片 ⇒ 不产 ICMP（对齐 gVisor `if pkt != nil`） |
| T24 | `icmp_suppression_set` | `orig.src == 0.0.0.0` / 目的组播 / 目的 255.255.255.255 ⇒ 不产 |
| T25 | `icmp_covers_non_udp_proto` | **TCP 分片**超时也发 11/1（内核无 proto 门）；type3/code3 的薄封装仍只对 UDP |
| T26 | `stats_snapshot_shape` | `snapshot()` 长度 15、既有键索引 `[0..=8]` 不变；**恒等式** `fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap` |
| T27 | `status_payload_new_keys` | `ServeInterceptBits` 序列化含 6 新键；**旧载荷（无 6 键）反序列化成功且取 0** |

### 8.2 出口侧 TX（F5-d）

| # | 用例 | 断言要点 |
|---|---|---|
| T28 | `tx_frag_rewrite_checksum_matches` | **直接取 smoltcp 产出的真分片**：对首片做 RFC 1624 增量更新后，客户端重组得到的整报文 UDP 校验和**校验通过**（并与「整报文重算」的对照值相等） |
| T29 | `tx_frag_src_consistent_across_fragments` | 同一报文全部分片改写后的 IP 源地址**一致**（= `orig_dst`） |
| T30 | `tx_non_first_fragment_never_touches_l4` | 非首片处理后：载荷**逐字节不变**、L4 校验和字段位置不变（只有 IP 源与 IP 校验和变） |
| T31 | `tx_frag_garbage_port_no_false_match` | 构造非首片载荷前 2 字节命中在册 `rw_port` ⇒ 仍**不得**改写（走表路径） |
| T32 | `tx_frag_missing_entry_drops_and_counts` | 表未命中 ⇒ 丢片 + `txFragDrop` 增 |
| T33 | `tx_frag_table_reclaimed_on_last_fragment` | 末片处理后表项消失；TTL/上限兜底各一条 |
| T34 | `tx_non_fragment_path_unchanged` | 非分片包的 `on_tx` 行为与现状逐字节一致（回归） |

### 8.3 客户端侧（F5-a/b/c）

| # | 用例 | 断言要点 |
|---|---|---|
| T35 | `client_udp_fragments_up_to_64k` | 客户端 `send_slice(4096B)` ⇒ TX 队列出现多个分片；`send_slice(65000B)` ⇒ 同样发出（回归 R4） |
| T36 | `client_udp_oversize_rejected` | `> 65507` ⇒ `Err(ConnErr::DatagramTooLarge)`，**不静默** |
| T37 | `client_reassembles_large_reply` | 出口侧 4096B 回复 → 客户端重组收到（回归 R9/R10 + F5-a/F5-c） |
| T38 | `client_multi_concurrent_reasm` | 两条并发大回复都能重组（F5-c 的 `reassembly-buffer-count-8`） |

### 8.4 批次级回归（收口前置）

- `cargo test --workspace --no-fail-fast`（F5-a 改特性位 ⇒ **必须全量重编译后复跑**）
- `cargo clippy --workspace --all-targets -D warnings`
- `tools/ci-local.sh`（基线/向量/词表/矩阵冒烟）+ OHOS 交叉 `cargo check --target aarch64-unknown-linux-ohos`
- 真机形态（用户触点，不在本棒）：手机核发 >1.2KB UDP 打到出口侧真实服务（QUIC / DNS-over-UDP 大响应 / TFTP 类），**两个方向都验**。

---

## 9. 设计门记录（dsh 外部评审）

### 9.1 轮次信息

| 项 | 值 |
|---|---|
| 轮次目录 | `/tmp/dsh-review/r28.6ghIaa`（`prompt.txt` / `output.md` / `stderr.log`） |
| 命令 | `cd /Users/zhaozhe/Documents/projects/homeway-rs && dsh --profile headless "$(cat …/prompt.txt)" > …/output.md 2> …/stderr.log` |
| **exit code** | **`exit=0`**（前台跑，捕获；`stderr.log` 1445 行 = 推理流，无错误） |
| 结果规模 | `output.md` **198 行**（已用 Read 工具读全，非截断转述） |
| 评审者自查 | 「未改仓库任何文件」；评审后 `git status` 仅 `?? docs/reviews/QK-design.md`（未发现 dsh 落仓文件） |
| 评审者验证手段 | 逐行核 `intercept/{mod.rs,nat.rs}`、`wgcore/{mod.rs,stackb.rs}`、`server/engine.rs`、`daemon/proto.rs`、`crates/homeway-cli/src/unified_cli.rs`、根 `Cargo.toml`；只读 smoltcp 0.14 / gVisor `39ed1f5` / `baseline/homeway`；并用 `/tmp` 内独立 rustc 片段验证设计给出的 `on_plain` 代码形态可编译 |
| 门结论（原文） | 「**需修订后再过门（有条件通过）**」——1 高 / 10 中 / 6 低 + 9 组「看过没问题」 |

### 9.2 评审原文摘要（逐条，21 条）

**① 安全面**

- **【高-1】TX 侧 `on_tx` 未纳入分析：反向大包被 `rewrite_src` 破坏（校验和 + 概率性载荷污染）**。位置 `intercept/mod.rs:1898-1917` / `nat.rs:213-226` / `nat.rs:159-183`、设计 §1-R9、§2-F5-a、§6.1、§8-T24。问题分三层：① 首片 `Ipv4View::parse` 读到真 L4 头 ⇒ 命中 `by_rw_port` ⇒ `rewrite_src` ⇒ `fix_l4_checksum` 用**首片长度**重算，覆盖 smoltcp 刚写好的**全报文正确校验和** ⇒ 客户端 `UdpRepr::parse` 校验失败**静默丢**；② 非首片 `v.src_port` = 载荷前 2 字节（垃圾），命中 `by_rw_port` 概率 ≈ 在册流数/65536（满表 ≈ 7.8%），命中后 IP 源被改成**另一条流的原始目的 IP** ⇒ 客户端重组键分裂 ⇒ 报文永不完成 + 占唯一重组槽 60s + 载荷 2 字节被改写；③ ⇒ **R9 的「1253–1472 返向通」是错的**、**F5-a 的「顺带修复反向路径 ≥1473」也是错的**、T24 会红。建议：新增 **F5-d**（非首片只重写 IP 源 + IP 校验和；首片用 **RFC 1624 增量更新**；同报文全部分片携带**相同**改写后 IP 源），并加 TX 侧分片改写单测。
- **【中-1】限频日志的「形态键」未定义、无表上限，可被对端诱发灌爆**。位置 §2-F2 / §3.1-A9。既有 `dial_fail_seen` 有明确纪律：键**不含源端点**（`mod.rs:832-835` 注释）、`len > 1024 ⇒ clear()`（`:2340-2342`）；内层 `src` 对已认证客户端完全可伪造 ⇒ 逐包刷新「首行」⇒ 灌爆 + HashMap 无界。建议写明键组成（不含 src）+ 表上限与清表口径。
- **【中-2】ICMP 面三处未对齐**。位置 §2-F4 / §5.1-§5.2、`nat.rs:267-292`。① `build_icmp_unreachable` 现含 `proto != 17 ⇒ None`，设计未说明内核是否保留该门——gVisor 对任意 proto 都发；② gVisor 抑制 `src == 0.0.0.0`（`icmp.go:645`），设计未提；③ 补「TCP 分片超时也发 11/1」用例。
- **【无问题】**：A2/A3/A4/A5/A6/A7/A10 的处置与残余（全局「淘汰最老不拒新」与 gVisor 同形已核；分片不建会话由 `route_plain` 单入口结构保证；ICMP 只走 `tx_out → route_encap` 只回已认证 peer；`is_fragment` RX 侧全仓仅 `:955` 一处，无旁路）。

**② 正确性**

- **【高-2】同【高-1】**（「重组后与正常包完全相同」在 **TX 方向**不成立；设计只验证了 RX 方向）。
- **【中-3】`frag_gate` 的校验集与「片载荷取哪一段」未写明（可被实现成越界/夹带）**。建议写死校验集与「片长 = `total_len - ihl`，尾随字节丢弃」，并给畸形输入单测。
- **【低-1】§4.2 偏移合法性缺一条：`payload_len == 0 && more == false`**（gVisor 对任意空片都 `first > last ⇒ ErrInvalidArgs`；不得把空末片当「末片定位器」）。
- **【低-2】F1.4「UDP 校验和 0 语义原样保留」措辞失真**——重组器不碰 L4，但随后 `rewrite_dst → fix_l4_checksum` 会重算并写非 0 值；这是「与正常包相同」的既有行为，但措辞会误导实现者加分片特判。
- **【无问题】**：偏移/末片判定、IP 头处理（`total_len`/`flags`/`fix_ip_checksum`）、**重组产物 > MTU 不会被栈 RX 拒**（已核）、与 `rewrite_dst` 的先后、`on_plain` 拆两段的**代码形态可编译**（评审者用 rustc 实测）、`debug_assert!` 的论证成立、R8 ✓。

**③ 有界性**

- **【中-4】`REASM_MAX_FRAGS = 64` 的「合法最大 53」依赖未写明的隐含前提**（对端 IP MTU = 1280）；出口无法验证对端 MTU，MTU 576 的合法发送方会产生 123 片 ⇒ 合法大 UDP 被拒。§3.4 把它归入「只在恶意/畸形输入上产生可观测差异」与事实不符。
- **【中-5】`REASM_MAX_BYTES` 与 `REASM_MAX_CTX` 数值近乎重合**（只差 64 字节）⇒ 字节闸几乎永不独立生效，T13 难构造。建议写明量化并改 T13 构造或改判为编译期断言。
- **【中-6】A8 只给「单上下文 O(64²)」，没给全局速率上界**（淘汰最老 ⇒ 攻击者可无限重建上下文）。建议补全局口径或把插入改为二分 + 相邻比较。
- **【低-3】`REASM_MAX_PER_SRC` 的论证自相矛盾**（§3.2 用「单缓冲 ⇒ 恒 ≤1」，A3 又用「伪造多源可绕过」否定配额）。
- **【无问题】**：其他上界都给数且自洽（每上下文 ≤65535 / 30s / 输出 ≤65535 / RSS 算术正确）；**清扫触发点足够**（`pump`/`pump_hold` 每拍调用、`close()` 清空）；F5-a 的 `FragmentsBuffer` 增量算术正确（+125 KiB，生产面确为 2 处）。

**④ 与 Q-B F7 的关系**

- **【无问题】**：两条洞都没复活，且有**结构**保证（`push` 只碰 `self.reasm`；建流唯一入口在 `route_plain`；`frag_gate` 的 `Part` 分支不触达 `by_five`/`tcp_new`/`udp_new`；`rewrite_dst` 只作用于已重组整包）。
- **【补充】**：「**但对称面没做**」——`rewrite_src` 在 TX 侧污染分片载荷/校验和；Q-B F7 只堵了 RX 半边，Q-K 的分析同样只看了 RX 半边。这正是本批必须补的一课。

**⑤ Go 对齐**

- **【中-7】§3.4/§4.1 的「重叠 = 整条丢弃（对齐 Go）」不准确**：gVisor 只在「片**越过**当前 hole 边界」时整条丢弃；若来片**完全落在已填充 hole 内**（含区间更小的重叠片），走 `currentHole.filled ⇒ continue` ⇒ **静默忽略**。⇒ §3.4 该行应改为「Go：越界重叠整条丢弃 / 内含重叠静默忽略；本设计：一律整条丢弃 ⇒ **比 Go 严**」；§4.3 漏了「内含但区间不同」子形态；须**登记为有意分歧**。
- **【中-8】ICMP 载荷长度与 Go 不一致**：gVisor 按 RFC 1812 发**尽可能多**（`min(MTU,576) − 8`，实测可达 ~548B），我们发固定 8B ⇒ 线上字节不同，「对齐 Go」不成立。建议改长度或登记为有意分歧。
- **【低-4】「gVisor 默认不限频」结论对、依据错**：`NewICMPRateLimiter` 初始化的是 1000/s、burst 50；「不限频」的真正原因是 `icmpRateLimitedTypes` 空集 ⇒ `allowICMPReply` 直接 `return true`。
- **【低-5】§5 理由 1「与 Go 逐项对齐」的说服力已被 R12 + Go 退役掏空**：唯一真实消费者是出口侧抓包/排障；建议把「出口侧可观测」提到首位、理由 1 降为「与历史 Go 行为同形（无现役消费方）」。
- **【无问题】**：§3.4 的「每源配额 / 片数上限 = 有意更严」定性恰当；F6 裁定合理。
- 另（综述）：超时 30s、内存 4/3 MiB 与队尾淘汰、非法/非 8 倍/越界整条丢弃、同区间重复片静默接受、超时 ICMP 11/1 仅首片在位、ICMP 只在超时（内存压力淘汰 `timedOut=false`）——**逐项复验 ✅**；但要求把「`created` 起算、**不续期**」写进文档（`Ctx.last` 的存在容易被实现成续期）。

**⑥ Go 直译痕迹**

- **【中-9】`push(&hdr, &pkt, now)` 把「头视图 + 整包」分开传 ⇒ 允许不自洽的调用**（gVisor 的 `Process(id, first, last, more, proto, pkt)` 正是这种形态，属直译痕迹）。建议 `parse(&pkt) -> Option<(Ipv4FragHdr, &[u8])>` 或 `FragSlice`。
- **【低-6】其余小项**：`Ctx.key` 与 HashMap 键重复存储；`Ctx.last` 用途未定义（若用于续期须与 gVisor 对齐并写明）；`PushOutcome::Dropped` 与 `sweep → Vec<Evicted>` 两套原因通道建议统一；`Piece{data: Vec<u8>}` 每片一次分配**可接受不必改**。
- **【无问题】**：`FragKey` newtype 化、`#[non_exhaustive]`、新模块 + `pub(crate)`、`sweep(now)` 可注入时基、`enum Frag` 单入口、`debug_assert!` 绊线、错误走类型、`snapshot()` 追加末位、`ServeInterceptBits` 全 `#[serde(default)]`——均符合工程原则，无 Go 包结构 1:1 映射。**笔误**：`homeway-cli` 路径应为 `crates/homeway-cli/…`。

**⑦ §1 的 R4/R9/R12 订正评估 + §5/§6 裁定**

- **R4：证据链扎实，采纳**（机制 + 算术 + 探针输出逐值吻合）；「据此把客户端侧动作从『可选』提到『必修』是正确的」。
- **R9：结论中「返向通」与「≥1473 只需 F5-a 即可修复」两处站不住**（见高-1）——「这不是『订正』，而是**结构推理未做实证**：设计只用探针验了客户端 TX 侧，反向路径从未被测量」。⇒ §6.3 的「两侧都要动」还漏了第三处（出口 TX 反重写）；T24 按现状会失败，须提升为「反向大包端到端」并显式依赖 F5-d。
- **R12：证据链扎实，采纳**；§5.4 强制登记这条负面事实是正确的。
- **裁定判断**：§6「是，纳入本批 F5」方向正确，但**必须加上 F5-d**，否则 F5-a 落地后新增的 ≥1473 分片流量会踩上一条**现网从未暴露过的**破坏路径；§5「做 type 11 code 1」可做，但需按【中-2】【中-8】收口。

**⑧ §7 判据登记完整性**

- **【无问题】**：§7.1「零编号判据行变更」复验成立（全仓 `分片` 仅 3 处命中，均非编号行）；§7.2 行格式符合该表既有多行形态（该表本就接纳「非编号判据行」条目）；§7.3 用「计数输入集」表登记 `fragDrop` 重定义 + additive + `tx_deferred` 单位，**正是该表政策要求的形态**。
- **【中-10】三处登记缺项**：① F5-a 改变了「出口 TX 侧现在真的会分片」这一 **wire 行为**（1 包变 N 包）及对客户端入站重组（`REASSEMBLY_BUFFER_COUNT=1`）的影响，§7 未登记；② `fragBad` 无独立计数 ⇒ 恒等式只能相减反推，验收方无法独立核对；③ `REASM_*` 六个常量是编译期还是进 config/status，§7 未表态。
- **【低】**：§7 说批注块在「**节首**」，实际在**登记表尾部**（与 Q-J/Q-F-B 的「最新一行」惯例一致）；措辞需订正。

**⑨ 评审者明确列出「看过，没发现问题」的部分**

§1 证据链 R1/R2/R3/R5/R6/R7/R10/R11/R13/R14（**逐值核对无误**）；F5 的依赖接线（根 `Cargo.toml` 是 workspace 依赖、feature 名与 `build.rs` 解析路径正确、`udp_send` 是唯一 UDP 发送点）；`Stats::snapshot()` 追加安全性（既有断言只用 `[0]`/`[1]`/`[2]`）；§3 其余数字自洽；§8 测试计划覆盖面（只缺「近 64 KiB 端到端」与「出口 TX 侧」两档）；§5/§6 裁定的**形式**符合本仓纪律；§7 的落点选择符合政策、无「该登记而未登记」的编号判据行。

### 9.3 逐条处置表

| # | 严重度 | 处置 | 落点（v2 变更） |
|---|---|---|---|
| 高-1 | **高** | **认同**（独立复核成立：`on_tx` 无分片门；`fix_l4_checksum` 用 `l4_len = pkt.len() - ihl`；smoltcp 在分片前已按整报文写头，`wire/udp.rs:286-309`） | **新增 F5-d**（§2）+ **R15**（§1）+ **R9 订正**（§1）+ §6.1/§6.3 重写 + T28–T34 新增 + §7.3 第 3 行登记 wire 形态变化 |
| 高-2 | **高** | **认同**（同高-1；TX 方向的「相同路径」不成立） | 同上 |
| 中-1 | 中 | **认同**（`mod.rs:832-835` 的键纪律 + `:2340-2342` 清表已核） | §2-F2 写死：键 = **纯 kind**、两个封闭具名字段（无 HashMap）、节流 1/100；A9 更新 |
| 中-2 | 中 | **认同**（`icmp.go:645-647` 已核；gVisor 不判 proto） | §2-F4：内核**去 proto 门**、薄封装保留 `proto==17`；补抑制集（`src==0.0.0.0` / 组播 / 255.255.255.255）；T25 新增 |
| 中-3 | 中 | **认同** | §2-F1.2 写死校验集与 `payload = &pkt[ihl..total_len]`；T11/T12 新增 |
| 中-4 | 中 | **认同**（§3.4 的归类确实不准） | §3.2 改依据为「**前提假设 MTU=1280** + 有意更严」；`fragLimit` 行文带片数维度；§3.4 与 §7.4 登记 |
| 中-5 | 中 | **认同**（64 × 65535 与 4 MiB 只差 64B，已复算） | §3.2 写入量化 + **编译期断言** `REASM_MAX_BYTES >= REASM_MAX_CTX * 65535`（对齐既有 `assert!(UDP_OUT_GATE < WATERMARK)` 形态）；T17 给出构造 |
| 中-6 | 中 | **认同** | §3.1-A8 改为**有序二分 + 相邻比较**（每片 O(log n)+O(1)）并补全局口径 |
| 中-7 | 中 | **认同**（复核 `reassembler.go:113-116`/`:127-130` + 填洞逻辑 ⇒ 填充空洞精确等于某片区间，「内含」即重复或子区间） | §3.4 表格该行改写 + §4.1 补实现细节 + §4.3 增「内含但区间不同」第三子形态 + §7.4 登记为**有意更严**；T6 新增 |
| 中-8 | 中 | **认同**（`icmp.go:745-772` 已核，实测上限 548B） | §5.3 登记为**有意分歧**（保持 8B，三条理由 + 注明将来的对齐改动点） |
| 中-9 | 中 | **认同**（工程原则 1） | §2-F1.2 改 `FragSlice`；`push(frag: FragSlice<'_>, now)`；`Frag::Part` 直接携带它 |
| 中-10 | 中 | **认同**（三项全采） | ①§7.3 第 3 行登记 wire 形态变化；②**加 `fragBad` 独立计数**（snapshot 9→14，恒等式四项全可独立读）；③§3.2 末行写明「全部编译期常量、不新增配置键、不进 status」 |
| 低-1 | 低 | **认同** | §4.2 增第 4 条（空末片同丢）+ T9 |
| 低-2 | 低 | **认同** | §2-F1.4 措辞改写（重组器不碰 L4；随后由既有 `rewrite_dst` 按与正常包相同规则处理） |
| 低-3 | 低 | **认同** | §3.1-A3 与 §3.2 统一为「公平性闸」 |
| 低-4 | 低 | **认同**（`icmp_rate_limit.go:21-27` + `ipv4.go:1847-1860` 已核） | §1-R11 依据订正 |
| 低-5 | 低 | **认同** | §5.2 理由重排（可观测性升为首要、对齐降为「历史同形（无现役消费方）」） |
| 低-6 | 低 | **认同**（除「`Piece` 每片一次分配」保持） | 删 `Ctx.last`（并写明不续期）、`DropReason` 统一（`Evicted` 复用）、路径笔误改正、§3.2 明「不续期」 |
| 中-7/⑧ 的「节首」 | 低 | **认同** | §2-F3 改为「**登记表尾部**批注块」 |
| ⑨ 无问题项（6 组） | — | **记录**，不动作 | §9.2 ⑨ |

**未采纳/保留意见**：无。21 条意见**全部认同**（0 条不认同、0 条部分认同），其中【高-1】为**结构性修订**（新增一个修复项 F5-d 并推翻三处 v1 结论）。

### 9.4 过门结论

**通过（附结构性修订）**——2026-10-08，dsh `r28.6ghIaa`，**exit=0**，`output.md` 198 行读完。

- 评审门给出的原结论是「需修订后再过门（有条件通过）」，1 高 / 10 中 / 6 低。**本文件 v2 已把 21 条意见全部并入**（§9.3），其中【高-1】带出 **F5-d** 这一新增必修项与 R9/R15 的结论订正；三处「有意更严/分歧」（每源配额、片数上限、内含重叠与冲突重复片、ICMP 载荷长度）已按门内要求**显式登记**；§7 的三处登记缺项已补齐。
- **入代码门前的前置**（第 2 棒必须满足）：
  1. F5-a（`fragmentation-buffer-size-65536`）**必须与 F5-d 同批落地**——单独落地会把「静默丢」换成「分成 N 片后校验和被写坏」。
  2. T28/T37 是**真分片对照**用例（取 smoltcp 产出的真分片验校验和）——不得用自造包替代。
  3. §7 的登记行与代码**同一 commit**。
- **需上报项**：无新增；本批的执行范围由「出口侧 RX 重组」扩为「RX 重组 + 出口 TX 反重写分片感知 + 客户端侧能力/可见性」三项，扩围依据 = §9.3 的【高-1】与 R4/R15——**主会话已授权按建议定序，故不单独请示**。

---

## 10. 实现注记（第 2 棒；代码门 r29 后的订正与偏差）

> 本节的每一处都是**实现期/代码门后**对 §2–§8 的订正或偏差说明；§1–§9 保留过门时的原文
> （仅在确属硬错误处就地订正并在本节点名——见下表 §10.2）。代码门证据见 `docs/reviews/QK.md`。

### 10.1 实现形态偏差（1 处）

| 设计（v2） | 实现 | 理由 |
|---|---|---|
| `enum PushOutcome { Pending, Done(Vec<u8>), Dropped(DropReason) }`（§2-F1.1） | `struct PushResult { done: Option<Vec<u8>>, dropped: Vec<Dropped> }` + `Dropped { reason, packets, src, icmp_orig }` | 一次 `push` 可**同时**产生两类事件（为腾额度淘汰旧上下文 + 本片被收下/完成/被拒），单值枚举表达不了；原因仍是**单一通道**（`Dropped` 列表），调用方一个循环记账。「恒等式由 `Stats::note_frag_drop` 单次调用结构保证」的设计意图不变。 |

### 10.2 代码门 r29 检出的缺陷（已改码，逐条）

| # | 门内严重度 | 问题 | 处置（改码点） | 证据 |
|---|---|---|---|---|
| 中-1 | 中 | `try_finish` 的 `(ihl + total) as u16` **静默回绕**（首片带 IP 选项 `ihl ≤ 60`、`total` 可达 65535 ⇒ 最大 65595）⇒ 产物被 `route_plain` 的 parse 拒而静默丢，却已计 `fragReasm` | `try_finish` 改三态 `Finish{Done,Pending,Oversize}`；`Oversize ⇒ drop_whole(Bad)`（连带计包数） | `reasm.rs::ip_options_preserved_and_oversize_dropped` ②（ihl=24 + total=65535 ⇒ Bad、2 包） |
| 低-1 | 低 | 更短的另一片末片可**改小** `total_len` ⇒ 已收齐却永不完成（gVisor 是 `ErrFragmentConflict` 整条丢弃） | ③ 判定前置「末片边界不可变」（`!mf && total != end ⇒ Overlap`）+ `debug_assert` | `reasm.rs::shorter_final_fragment_conflicts`（3 包连带、整条丢弃） |
| 低-2 | 低 | TX 分片表**插入路径**可瞬时到 `TX_FRAG_MAX + 1`（表满且条目全新鲜时） | 首片登记路径：先 TTL 清扫，再 `while len >= MAX ⇒ 淘汰最老`（为本次 insert 留位） | `intercept::mod::tx_frag_table_cap_on_insert_path` |
| 低-3 | 低 | T17（全局字节闸）无真用例 | **改判为「编译期断言 + 不可达证明」**（§8.1 T17 原文允许该口径）：任何合法写入后 `Σbytes ≤ 64 × 65535 = 4,194,240 < 4 MiB` ⇒ 该分支按不变量不可达；实现保留为纯防御面（防将来放宽上限/记账被改坏）。§3.2 的「窄窗口触发」措辞已就地订正 | 编译期断言 `const _: () = assert!(REASM_MAX_BYTES >= REASM_MAX_CTX * 65535);` + `make_room` 文档证明 |
| 低-4 | 低 | 两条新限频日志未在 `INTEROP-CRITERIA.md` 留痕（同批同节惯例） | 批注块补「additive 观测行 2 条」 | `docs/INTEROP-CRITERIA.md` 判据变更记录尾部批注块 |
| 低-5 | 低 | §3.1-A10 的「放大比 < 1」数值不成立（最小诱发 28B ⇒ 回报 56B ≈ 2×） | **就地订正**为「单次 ≤2×、结构性速率 ≤2.1 报文/s ≈ 120B/s」（安全结论不变：只回已认证 peer、不经公网） | 本文件 §3.1-A10 |
| 低-6 | 低 | 死代码/冗余转发：`Ipv4FragHdr::frag_key()` 零调用、`Reassembler::count_src_of` 只是转发 | 删 `frag_key`；`count_src` 直接作日志点调用（`pub`） | grep 零残留 |
| 低-7 | 低 | `PushResult` 注释「与 `dropped` 互斥」与实际不符（字节闸淘汰 + 本条可同时发生） | 注释改为「**可以同时非空**」 | `reasm.rs` 结构体文档 |
| 低-8 | 低 | `push` 早退路径丢弃已累积的 `dropped`（今天不可达，但结构易碎） | 引入 `drop_whole_into(..., out: &mut Vec<Dropped>)`，全部早退路径合并返回 | `reasm.rs` §③/§④/§⑥ |

**门结论**：dsh `r29.3l5A8V` **exit=0**，「可以过代码门（无阻断项）」，**0 高** / 1 中 / 8 低 —— **全部处置**（改码 5 条 + 登记 1 条 + 订正 2 条 + 注释 1 条，见上表）。

### 10.3 测试计划落地对照（T1–T38）

| 设计编号 | 落地用例（`#[test]`） | 状态 |
|---|---|---|
| T1/T19/T24/T37 | `intercept::tests::large_udp_roundtrip_both_directions`（真分片双向端到端 + 原目的端口上游可见 + `fragReasm==1`/`fragDrop==0`/`txFragDrop==0`） | ✅ 真用例 |
| T2/T3/T4/T5/T6/T7/T8/T9/T10/T12/T13/T14/T15/T16 | `reasm::tests::{two_fragments_and_out_of_order, duplicate_identical_ignored, conflicting_duplicate_drops_whole, overlap_subforms_drop_whole, conflicting_final_drops_whole, bad_offsets_drop_whole, trailing_bytes_excluded, timeout_evicts_and_not_renewed, ctx_limit_evicts_oldest, per_src_limit_rejects_new, frag_count_limit_drops_whole}` | ✅ |
| T11 | `nat::tests::frag_header_parse_malformed_inputs` | ✅ |
| T17 | 编译期断言 + 不可达证明（见 10.2 低-3） | ⚠️ 设计允许的替代口径 |
| T18/T20 | `intercept::tests::fragments_never_touch_flow_table` + `fragment_never_injected_into_existing_flow` | ✅ |
| T21 | `intercept::tests::reasm_tcp_fragments_deliver` | ✅ |
| T22/T23 | `intercept::tests::icmp_time_exceeded_on_timeout` / `icmp_not_sent_without_first_fragment_or_suppressed` | ✅ |
| T24（抑制集） | 同 `icmp_not_sent_without_first_fragment_or_suppressed` ②（三态） | ✅ |
| T25 | `intercept::tests::icmp_covers_non_udp_proto` + `nat::tests::icmp_core_type_code_and_suppression` | ✅ |
| T26 | `intercept::tests::stats_snapshot_shape_and_frag_identity` | ✅ |
| T27 | `daemon::proto::tests::intercept_bits_new_keys_and_backward_compat` | ✅ |
| T28/T29 | `intercept::tests::tx_frag_incremental_checksum_matches_full_recompute`（**真分片**重组后 UDP 校验和自洽 + 与 `rewrite_src` 全量重算逐字节相等） | ✅ |
| T30/T31 | `intercept::tests::tx_non_first_fragment_touches_only_ip_source`（载荷前 2 字节 = 在册 rw_port 也不误改写） | ✅ |
| T32 | `intercept::tests::tx_frag_missing_entry_drops_and_counts` | ✅ |
| T33 | `intercept::tests::tx_frag_table_ttl_and_cap` + `tx_frag_table_cap_on_insert_path` | ✅（含门内低-2 的插入路径） |
| T34 | `intercept::tests::tx_non_fragment_path_unchanged` | ✅ |
| T35 | `intercept::tests::client_udp_fragments_up_to_64k`（4096B 与 65000B 真分片） | ✅ |
| T36 | `wgcore::tests::udp_payload_gate_boundaries`（纯函数面；引擎侧调用点在 `udp_send` 首部） | ✅（函数级） |
| T38 | `intercept::tests::client_multi_concurrent_reasm`（交错注入两报文真分片——**已实测判别力**：把 feature 换回 `reassembly-buffer-count-1` 该用例红） | ✅ |
| 中-1 补 | `reasm::tests::ip_options_preserved_and_oversize_dropped`（ihl=24 两态） | ✅ 新增 |
| 低-1 补 | `reasm::tests::shorter_final_fragment_conflicts` | ✅ 新增 |

**未覆盖（登记残余）**：跨报文乱序（两报文分片在隧道上真交错）无 E2E 用例——真栈的 `Fragmenter` 单缓冲使该形态只能在测试里手工构造（已由 T38 的交错注入覆盖客户端侧；出口侧 `Ctx` 是 per-key 结构，天然隔离）。
