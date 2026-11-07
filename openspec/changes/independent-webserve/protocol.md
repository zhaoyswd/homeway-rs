# WebServe v1 wire 附录

本附录固定新增 wire，供两端独立实现与生成夹具；现有 hmw2/rl1、hr-reg4、中继信封/控制帧、普通服务 tag 和 TLS/ALPN 均不变。实施时将定值正反向量加入 `fixtures/webserve/` 并更新全目录 SHA256SUMS，未生成前不标记夹具已验。

## 1. hww1 与磁盘凭证

token = ASCII `hww1`（4B）|| base64url-no-pad(body)，body 恰116B；编码正文恰155字符，整串159字符。仅允许首尾Unicode空白trim，拒绝内部空白、`=`、非URL字母表、非规范尾位、多余或不足字节。未知 `hww*` 版本是 UnsupportedVersion；编码/长度/校验错误是 Malformed，不回显原串。

| body offset | 长度 | 字段 |
|---|---|---|
| 0 | 32 | 目标 X25519 public key（与当前出口/中继注册身份同源） |
| 32 | 32 | 目标 Ed25519 RPK public key |
| 64 | 16 | 独立 Web key id，CSPRNG |
| 80 | 32 | 独立 Web secret，CSPRNG |
| 112 | 4 | SHA256(ASCII `hww1` || body[0..112]) 的前4B |

校验值只检输入损坏，不是签名/授权。token 无网络地址、不带 hmw2 secret；本机验证完整身份/RPK/key id/secret，远程必须通过目标TLS钉定与管理认证。

`web/auth.key` 恰53B：ASCII `HWWK`(4) || version=0x01(1) || key id(16) || secret(32)。其他长度/magic/version拒绝，不自动覆盖损坏文件。目录0700、文件0600；不存在时最小初始化，轮换同目录临时文件原子替换；失败保留旧凭证。

## 2. QUIC 管理认证

首条bidi客户端首先发送固定 ClientHello；其他流不得并发抢首流。所有多字节整数BE，所有magic为ASCII。客户端保持原RPK钉定TLS配置、无ALPN；共享出口在原两字节read_head处识别`W1`。

| 方向 | 帧 | 逐字节布局 | 总长 |
|---|---|---|---|
| C→S | ClientHello | `W1` || version=0x01 || key id(16) | 19B |
| S→C | Challenge | `WC` || version=0x01 || nonce(16) | 19B |
| C→S | Proof | `WP` || version=0x01 || key id(16) || MAC(32) | 51B |
| S→C | Accept | `WA` || version=0x01 | 3B |

TLS exporter = `export_keying_material`，label=ASCII `homeway-web-exporter-v1`、context为空、输出32B。MAC = HMAC-SHA256(Web secret, ASCII `homeway-web-auth-v1` || version byte 0x01 || key id(16) || nonce(16) || exporter(32))。Proof的key id必须等于Hello，nonce由本连接服务端CSPRNG生成且仅用于一次Proof。重放到新TLS连接exporter不同，即使nonce重复也不能通过；不能使用network-token secret试秘。

从接受本连接开始，等待首流/Hello/Proof、内存凭证验证、固定代际腿确认共享绝对5秒预算；客户端TLS连接+认证总预算10秒。未知key id或MAC错误用统一认证关闭码，不区分吊销/不存在。只有目标认证、管理名额与入口入队可用、并且中继腿真实提升成功才发送Accept；无HTTP入口不会拒绝合法管理认证。

| QUIC application close code | 语义 |
|---|---|
| 0x70 | 认证失败（含未知/已失效key id、MAC错误） |
| 0x71 | 管理协议非法（magic、version、长度或额外bidi） |
| 0x72 | 管理认证总预算超时 |
| 0x73 | 管理资源/入口额度不足 |
| 0x74 | 已认证grant撤销 |
| 0x75 | 中继腿确认失败（代际/腿失效/配额） |
| 0x76 | 管理能力不可用（无宿主/无凭证/终端禁用） |

关闭reason只用固定脱敏字符串。短读/EOF按连接终止回收，不在同连接重试认证。管理连接只允许首条bidi；额外bidi触发0x71关闭，DATAGRAM不绑定且永不进入设备/普通服务。旧服务端不认识W1时可能以旧BadData关闭，客户端映射为“目标尚不支持Web管理或认证失败，请升级/核对凭证”，不伪造成功。

## 3. 管理认证后的终端流

每帧 `opcode:u8 || length:u32BE || payload[length]`，header恰5B，length≤65536；先检查长度再分配/读体。不足header/体按异常EOF回收；未知opcode、错方向/长度/状态报协议错误并关闭。该层由CLI管理worker实现，QUIC pump只搬字节。

| opcode | 名称/方向 | payload |
|---|---|---|
| 0x01 | OPEN C→S | version=0x01 || cols:u16BE || rows:u16BE，恰5B |
| 0x02 | INPUT C→S | 原始终端字节，1..65536B |
| 0x03 | RESIZE C→S | cols:u16BE || rows:u16BE，恰4B |
| 0x04 | OUTPUT S→C | 原始终端字节，1..65536B |
| 0x05 | CLOSE 双向 | 空payload，幂等结束本界面 |
| 0x06 | ERROR S→C | code:u8 || UTF-8 message，整体1..513B |
| 0x07 | EXIT S→C | exit status:i32BE，恰4B；取不到正常退出码用-1 |

Accept后只有一次OPEN允许创建PTY；OPEN前INPUT/RESIZE、重复OPEN、客户端OUTPUT/ERROR/EXIT均非法。OPEN/RESIZE尺寸cols∈[1,1000]、rows∈[1,500]，越界拒绝，不做溢出截断。等待OPEN最多30秒（与未消费票据寿命一致），未经OPEN不创建PTY。

ERROR code：1=协议非法，2=终端禁用/不支持，3=资源忙，4=PTY或IO失败，5=本机控制面不可用，6=凭证撤销。message为固定或脱敏可行动文案，不能含token/secret/原请求体。正常退出可EXIT后CLOSE；对端断开无需强求写完结束帧。每向积压≤2MiB，顺序不变、不静默丢字节；持续背压10秒关闭本界面。

## 4. HTTP 与 WebSocket 消息

- `POST /api/login`：JSON对象 `{ "token": "hww1..." }`，body≤4096B；成功200返回 `{ "ticket": "<base64url-no-pad 32B>", "expires_in": 30 }`。票据43字符、随机32B、一次消费，与目标/epoch/grant绑定；在途登录+票据+活跃界面统一≤8。认证失败401，非法请求400，容量/限流429，目标暂不可用503；错误正文仅固定code/message。
- `GET /ws`：HTTP/1.1升级，线级frame/message均≤65536B。首帧必须为≤2048B文本JSON `{ "type":"auth", "ticket":"...", "cols":80, "rows":24 }`，5秒绝对期限；尺寸按上述范围，消费票据成功再发送OPEN/创建本地PTY。服务端成功文本 `{ "type":"ready" }`。
- 已认证binary消息为原始INPUT，服务端binary为OUTPUT；文本仅 `{ "type":"resize", "cols":N, "rows":N }` 或 `{ "type":"close" }`。服务端错误文本 `{ "type":"error", "code":"固定码", "message":"脱敏文案" }`；退出文本 `{ "type":"exit", "status":N }`。
- WS关闭码：1000正常、1002协议非法、1008认证/Origin策略失败、1009超限、1011内部IO/目标失联、1013资源忙。未经认证不回显票据或目标身份。标准Ping/Pong仅用于活性，不视为认证或终端输入。
- HTTP头≤16KiB/32字段且绝对5秒，body绝对5秒，每请求总15秒；HTTP禁keep-alive，WS升级后由WS期限接管。Host/Origin按design解析归一和allowlist处理；所有响应含no-store，静态页CSP只加载自身打包资源。
