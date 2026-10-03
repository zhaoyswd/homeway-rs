#!/bin/zsh
# gen-fuzz-seeds.sh — fuzz corpus 种子运行期展开（R5-5b；评审 ②-5：以 fixtures 为
# 单源，不重复入库字节；CI 校验展开 hash——见 ci-local.sh 第 4.5 步）。
#
# 从 fixtures/vectors/*.json 提取字节字段（token/relay/stun_sped/reg/files_frames），
# 展开到 fuzz/corpus.seeds/<目标>/（**纯种子区**——cargo-fuzz 的 corpus/ 是运行期增长
# 目录会污染，种子 hash 只对本目录算）。跑 fuzz 前把种子拷进 corpus/：
#   cp fuzz/corpus.seeds/<t>/* fuzz/corpus/<t>/ 2>/dev/null || true
# 展开汇总 hash 打印到 stdout 供 CI 比对（种子文件不变时应稳定）。
#
# 帧形口径（第二道门 中-9 整改——原脚本对控制面目标灌的是错帧）：
#   - fuzz_relay_ctl 的 CtlDecoder 吃 TCP 流形 `[2B BE len ≤256][子类型+体]`；
#     decode_* 族吃裸消息。两形态都产，另产「两消息首尾相接 + 尾部半条」流
#     （drain 边界形态——随机输入过长度闸概率 ~2e-3，不用种子钉住观测不到）。
#   - fuzz_leg_frame 的 RREG 报文走腿帧 `[0xBB][3][payload]`（无长度前缀）。
#   - fuzz_probe 吃真探测响应（HWR 头 + nonce + build + flags + 端点列表段）。
# 手工合成样本（设计 §2.3；第二道门 低-21 整改）：UPnP 713 body / M-SEARCH /
# DNS 查询应答 / 内层 IPv4 头——三个原零种子目标补齐。
# 注意：files 的 70KB 跨 u16 样本会展开（>4096）——libFuzzer 需 `-max_len=262160`
# 才吃得到（默认 4096 会拒收长种子）。
set -euo pipefail

REPO_ROOT="${0:h:A:h}"
OUT="$REPO_ROOT/fuzz/corpus.seeds"

python3 - "$REPO_ROOT" "$OUT" <<'PY'
import json, os, sys, hashlib, struct

root, out = sys.argv[1], sys.argv[2]
vec = os.path.join(root, "fixtures/vectors")

def unhex(s):
    return bytes.fromhex(s) if s else b""

def write(target, name, data: bytes):
    d = os.path.join(out, target)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name), "wb") as f:
        f.write(data)

n = 0
# token：正样本串 + 负例串（errors[].input 是整串 token——第二道门 中-12 整改：
# 原读 body_b64 字段恒空转，负例种子一条都没产）
t = json.load(open(os.path.join(vec, "token.json")))
for i, c in enumerate(t.get("cases", [])):
    if "token" in c:
        write("fuzz_token", f"tok{i}", c["token"].encode())
        n += 1
for i, c in enumerate(t.get("errors", [])):
    inp = c.get("input") or ""
    if not inp:
        continue
    write("fuzz_token", f"tokerr{i}", inp.encode())
    n += 1

# relay：控制消息（裸形态喂 decode_*）+ TCP 流形态（[2B BE len][msg] 喂 CtlDecoder）
# + 「两消息 + 尾部半条」流（drain 边界骨架）
r = json.load(open(os.path.join(vec, "relay.json")))
def ctl_frame(msg: bytes) -> bytes:
    return len(msg).to_bytes(2, "big") + msg
for i, c in enumerate(r.get("cases", [])):
    wire = unhex(c.get("wire", ""))
    if not wire:
        continue
    write("fuzz_relay_ctl", f"ctlraw{i}", wire)          # 裸消息（decode_* 消费形态）
    if len(wire) <= 256:
        write("fuzz_relay_ctl", f"ctl{i}", ctl_frame(wire))  # TCP 流单消息
        n += 2
    else:
        n += 1
# 两消息首尾相接 + 尾部半条（长度行 0x0008 只给 2B 体——半帧滞留缓冲形态）
if r.get("cases"):
    m1 = unhex(r["cases"][0]["wire"])
    m2 = unhex(r["cases"][1]["wire"]) if len(r["cases"]) > 1 else m1
    if m1 and m2:
        flow = ctl_frame(m1) + ctl_frame(m2) + bytes([0x00, 0x08, 0x02])
        write("fuzz_relay_ctl", "ctl-two-and-half", flow)
        n += 1
for key in ("dh", "nonce", "cookie"):
    v = r.get(key)
    if isinstance(v, str):
        write("fuzz_relay_ctl", key, unhex(v))
        n += 1

# stun_sped：请求/应答/帧
ss = json.load(open(os.path.join(vec, "stun_sped.json")))
for i, c in enumerate(ss["stun"]["requests"]):
    write("fuzz_probe", f"stunreq{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["stun"]["responses"]):
    write("fuzz_probe", f"stunresp{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["sped"]["controls"]):
    write("fuzz_speedtest", f"ctl{i}", unhex(c["wire"]))
    n += 1
for i, c in enumerate(ss["sped"]["datas"]):
    # 65535 样本超 cargo-fuzz 默认 max_len——1B/1400B/70KB 都留（跑时 -max_len=262160）
    if c["len"] <= 262160:
        write("fuzz_speedtest", f"data{i}", unhex(c["wire"]))
        n += 1

# 真探测响应（第二道门 高-3 整改：原 fuzz_probe 只有 STUN 字节，nonce 门后深层零引导）。
# 形态 = [HWR][ver=1][type=1][nonce 8B][build_len][build][flags]([cnt][18B 条目]*)
def probe_resp(nonce: bytes, build: bytes, flags: int, endpoints: list) -> bytes:
    b = b"HWR" + bytes([1, 1]) + nonce + bytes([len(build)]) + build + bytes([flags])
    if not endpoints:
        return b
    b += bytes([len(endpoints)])
    for *ip4, port in endpoints:
        b += bytes(10) + b"\xff\xff" + bytes(ip4) + port.to_bytes(2, "big")
    return b

write("fuzz_probe", "resp-list", probe_resp(b"\x0a" * 8, b"homeway-rs", 0b11,
      [(127, 0, 0, 1, 41641), (192, 168, 3, 12, 42661)]))
write("fuzz_probe", "resp-old", probe_resp(b"\x00" * 8, b"old-exit", 0b01, []))
write("fuzz_probe", "resp-utf8", probe_resp(b"\x11" * 8, "回家/出gå".encode(), 0b10, []))
n += 3

# reg：报文字节（腿帧族——[0xBB][3][payload]，无长度前缀；中-9 整改）
g = json.load(open(os.path.join(vec, "reg.json")))
for i, c in enumerate(g.get("cases", [])):
    wire = unhex(c.get("wire", ""))
    if wire:
        write("fuzz_leg_frame", f"reg{i}", bytes([0xBB, 3]) + wire)
        n += 1

# files_frames：4B 前缀帧（含 70KB 跨 u16 样本——低-22 整改：不再被 4096 过滤器丢掉）
ff = json.load(open(os.path.join(vec, "files_frames.json")))
for i, c in enumerate(ff.get("cases", []) if isinstance(ff.get("cases"), list) else ff.get("frames", [])):
    wire = c.get("wire_hex") or c.get("wire") or ""
    if isinstance(wire, str) and wire:
        b = unhex(wire)
        if len(b) <= 262160:
            write("fuzz_files", f"frame{i}", b)
            n += 1

# ---------- 手工合成样本（设计 §2.3；低-21 整改：三个零种子目标补齐） ----------

# fuzz_upnp（设计定的最高优先面，原本一个种子都没有）
msearch = (b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n"
           b'MAN: "ssdp:discover"\r\nMX: 2\r\nST: upnp:rootdevice\r\n\r\n')
write("fuzz_upnp", "msearch", msearch)
soap_713 = (b"<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\">"
            b"<s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>"
            b"<detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\">"
            b"<errorCode>713</errorCode><errorDescription>SpecifiedArrayIndexInvalid</errorDescription>"
            b"</UPnPError></detail></s:Fault></s:Body></s:Envelope>")
write("fuzz_upnp", "soap-713", soap_713)
desc_xml = (b"<root><device><serviceList><service><serviceType>WANIPConnection:2</serviceType>"
            b"<controlURL>/udp?control?url=1</controlURL></service></serviceList></device></root>")
write("fuzz_upnp", "desc-xml", desc_xml)
write("fuzz_upnp", "url", b"http://192.168.3.1:49152/root.xml")
n += 4

# fuzz_dns（RFC1035 查询 + 带答案应答——截断/压缩指针形态靠变异）
dns_query = (b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00"
             b"\x07example\x03com\x00\x00\x01\x00\x01")
write("fuzz_dns", "query-a", dns_query)
dns_resp = (b"\x12\x34\x81\x80\x00\x01\x00\x01\x00\x00\x00\x00"
            b"\x07example\x03com\x00\x00\x01\x00\x01\xc0\x0c\x00\x01\x00\x01"
            b"\x00\x00\x01\x2c\x00\x04\x5d\xb8\xd8\x22")
write("fuzz_dns", "resp-a", dns_resp)
write("fuzz_dns", "tcp-framed", (len(dns_resp)).to_bytes(2, "big") + dns_resp)
n += 3

# fuzz_inner_pkt（IPv4+UDP / IPv4+TCP SYN 最小骨架）
def v4_pkt(proto: int, body: bytes) -> bytes:
    total = 20 + len(body)
    hdr = bytes([0x45, 0x00]) + total.to_bytes(2, "big") + b"\x00\x01\x00\x00\x40" + bytes([proto]) + b"\x00\x00"
    hdr += bytes([100, 64, 213, 172]) + bytes([100, 64, 255, 1])
    hdr = hdr[:10] + struct.pack("!H", 0) + hdr[12:]  # 占位校验和（解析不校验）
    return hdr + body
udp_body = struct.pack("!HHHH", 53210, 53, 8, 0)
write("fuzz_inner_pkt", "v4-udp", v4_pkt(17, udp_body))
tcp_syn = struct.pack("!HHIIBBHHH", 53211, 443, 1, 0, (5 << 4), 0x02, 65535, 1024, 0)
write("fuzz_inner_pkt", "v4-tcp-syn", v4_pkt(6, tcp_syn))
n += 2

print(f"seeds={n}", file=sys.stderr)

# 汇总 hash（种子文件内容级——文件名参与排序后稳定）
h = hashlib.sha256()
for target in sorted(os.listdir(out)):
    d = os.path.join(out, target)
    if not os.path.isdir(d):
        continue
    for f in sorted(os.listdir(d)):
        h.update(target.encode())
        h.update(f.encode())
        h.update(open(os.path.join(d, f), "rb").read())
print(h.hexdigest())
PY
