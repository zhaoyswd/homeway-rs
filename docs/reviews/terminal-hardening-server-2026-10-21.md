# 终端服务端与 VT 整改（2026-10-21）

范围：`crates/homeway-core/src/term/`、显式 v5 夹具与判据登记。用户授权修复现有问题，
明确不使用 review skill；本记录按 ROADMAP 两道门分别以实现者与评审者视角结构化自评。
不提交、不推送、不发版，不触碰两台生产出口、冻结 baseline 或原有 v4 golden 文件。

## 门一：技术评审

### 设计与对齐

- 不在物理历史下标上打补丁：新增独立 historyEpoch/historyBase。真实 PTY 仿真入口记录
  滚动/裁剪，满容量仍推进绝对行号；FETCH 直接读取当前 Alacritty 历史，不从旧存活区
  猜测退出行。重排/清历史等破坏性变化换 epoch，主屏与备用屏不能共用缓存身份。
- surface v5 公共头 = `ver:u8/revision:u32/cols:u16/rows:u16/seq:u64/epoch:u64/base:u64`。
  SNAP/DIFF 成功入队才推进 seq，FETCH 不推进；FETCH 请求升为 epoch/from/count，
  共18B。客户端缺号请求快照，旧 epoch FETCH 由服务请求新快照恢复。
- cell v2 行头 = y:u16/wraps:u8，完整格 symbol 后增加 width:u8（0/1/2）。OSC调色板与
  默认色覆盖转换为显式 RGB，无须另发全量256色表；颜色变化强制行 damage。
- response_owner 为 active 的纯 raw 腿；否则服务端拥有应答权。自动应答、失焦、hover、
  key release 不应反过来抢走活动尺寸。OSC52 load 与常规应答使用同一归属规则。
- 同步输出沿用 VTE 的150ms deadline，由宿主等待循环驱动到期 stop_sync，不等待新字节。
- 粘贴先在有界事务内拼接，末片一次持 PTY 写锁提交完整 bracketed 首尾；跨腿输入不穿插，
  未完成/超限/断开事务无任何开括号进入 PTY，避免遗留粘贴状态。
- FETCH、镜像与视口均需字节及格数双门；单份快照不可入队应明确终局，不可重试放大。
  保留每会话一个投递线程；取一致性材质在服务锁内，逐腿建体/压缩/分片在锁外。

### 风险与结论

- wire 是显式不兼容升级，必须协调 APP surface 消费端；raw CLI 帧族不变。
- 历史入口必须包括 LF/newline、自动换行、SU、顶行DL；resize/reflow、ED3、RIS、
  DECCOLM 换 epoch。单列宽字符与单行滚动区的复杂组合保守换 epoch，宁可重建不串行。
- 不能只观察历史长度差：容量满后长度不变。隐式滚动观察 VT 行存储旋转，显式入口直接
  记真实数量；单行/零历史导致存储不旋转的 LF/ASCII wrap 另按实际动作计数。
- 不引入新异步运行时、额外全局锁或 Go 接口仿写。结论：按该方案实施，新增行为须登记。

## 门二：代码评审

### 已整改与复核

- 裁剪后行号仍单调，老 from 不会误命中新的物理行；反例在满历史后重写将退出的顶行，
  FETCH 得到真实新内容，不把先前存活区的旧内容追加成历史。
- 逐腿 seq 只在完整分片组成功入队后提交；队列失败不推进。淘汰仅删除完整待发 surface
  更新组，保留 STATE/CLIPBOARD/控制帧；在途组已被 writer 整组取走，不切断半组。
- 快照材质同拍共享 Arc，dirty 行只编码一次；逐腿 body 编码、gzip 与分片移到服务锁外，
  借用共享材质直接建体，移除逐腿 grid/mirror/row_bytes 临时深拷贝。
- 仍留在锁内的部分：VT 一致性读/指纹提交，以及快照 grid 和按真实字节截断的镜像取材。
  这是当前单锁架构的有界临界区，不宣称所有编码均锁外；未为局部优化重构会话锁体系。
- FETCH 同受 pending_cap 和 queue_bytes 约束；镜像从最近行向前按实际编码大小缩窗，
  第一行放不下就0行。视口本身超 pending_cap、或压缩快照整组超 queue_bytes，发送
  surface_unavailable + ENDED，避免永远无法成功的全量重试。
- 服务与客户端统一128Ki可视格上限（列1000/行500的分量上限不变），HELLO/RESIZE
  超面积明确错误；快照视口+镜像合计、单份 FETCH 展开也各最多128Ki格，阻断空白RLE放大。
  新面积归一测试曾查出先窄化 u16 导致小列数回绕，已改为 usize 先取 min 再窄化。
- 慢 writer 只记观测，不升级成更大的快照；surface writer 使用配置的写超时。
- 表面输入和 raw 自动应答共享 response_owner；程序 OSC 默认色覆盖不会被宿主主题更新
  偷换，OSC复位/RIS恢复宿主默认。OSC52 读只由归属方应答，写仍正常分发 surface。
- 分片粘贴最多1MiB，10s未完成取消，首片捕获 bracketed 模式；断腿清除未提交事务。
  完整提交沿用 PTY 有界/可取消背压写机制，KILL/进程退出仍能解除阻塞。
- 独立交叉复核补正：surface_loop 原有 `woke || flush_sync_pending` 的短路会使持续唤醒
  饥饿10s事务维护，已改为每轮无条件维护；真实服务测试保持3ms持续wake，要求已过期
  事务1s内释放所有权。镜像调用点的视口+镜像总格限制之外，helper自身也加128Ki防绕过，
  1000列/200条空白历史、无字节预算压力时 FETCH/镜像均最多131行。
- 主会话最终复核补正：OSC颜色变更只force_full会遗留同epoch深历史RGB缓存。新增独立
  材质失效操作只推进epoch、保留base；主/备用屏共享颜色变更共同换代，重复同值/重复无
  override复位不换代。新增满容量历史FETCH回归检查文本/绝对行号不变而RGB更新、备用屏
  改palette使主屏失效，以及OSC104/110/111恢复符号颜色与宿主主题查询值。
- codec 拒绝0长/越界RLE、非法width、尾随字节；冻结 v4 夹具只在测试中显式转换。
  v5独立定值向量精确检查 seq/epoch/base/wraps/真实宽度，不以旧digest归一替代验证。
- Rust形态检查：新增枚举承担 Pending 材质形态，复用现有 Arc/PTY 写序锁；没有字符串错误
  替代类型错误、没有无消费者导出、没有逐行移植 Go 包结构。

### 验证证据

- `cargo test -p homeway-core --lib term:: -- --test-threads=4`：**194通过/0失败**（17.02s）。
  新增覆盖 v5定值向量、满容量裁剪/真实退出行、主备用屏与重排epoch、空闲同步更新，
  宽字符/wrap/OSC颜色、主题覆盖复位、单行零历史滚动、混合raw/surface应答归属、
  原子粘贴与断开取消、材质字节预算、队列组淘汰、超预算明确终局。
- 最终借用编码收敛后 codec专项15/0、材质预算单例1/0；再全term194/0与all-targets clippy通过。
- 动态颜色深历史补正新增2例后：全VT **38/0**（含新palette/default色历史FETCH例）、
  codec **15/0**、all-targets clippy及diff-check通过；本次不重复主会话的全仓运行。
- `cargo clippy -p homeway-core --all-targets -- -D warnings`：通过。
- `git diff --check`（term/登记/夹具）：通过。
- `cd fixtures && shasum -a 256 -c SHA256SUMS`：通过；原有全部夹具不改，新增v5向量入清单。

### 边界

本批服务专项只在本机临时 UDS/openpty 验证；未把 Linux、真机、生产验收写成通过。
跨仓统一测试、OHOS实验核构建及APP渲染回归由主会话汇总；本记录不是部署许可。
v5必须协调升级客户端与服务，未实现生产v4兼容或自动降级；raw终端消费者仍按既有协议工作。
