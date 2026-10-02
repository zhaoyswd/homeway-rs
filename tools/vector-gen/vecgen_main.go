//go:build ignore

// vecgen_main.go — homeway-rs 对照向量生成器（R0.4）。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/ 的**模板**，由 tools/gen-vectors.sh 拷进
// baseline 克隆的 clientcore/internal/wtransport/ 再 `go run`，**绝不 commit 进克隆**（克隆是
// gitignore 的临时区）。放在包目录内 + `//go:build ignore`（stringer 惯用法）⇒ 可直调本包
// 未导出函数（deriveKey/deriveDevTag），向量因此走的是**生产真源**而非旁路重写。
//
// 产出（outdir 下三个 JSON，内容确定性——不含时间戳，重跑字节一致，供 diff 门禁）：
//   token.json       hmw1 编解码 + 错误分类（corrupted/unsupported_version/malformed）
//   tunnel_addr.json DeriveTunnelIP / DeriveTunIP（含守卫命中样本：hw-app 与 hw-tun 撞车的再散列路径）
//   identity.json    master+peerID → WG 私钥/公钥；master → devTag（含经 LoadOrCreateIdentity 全路径交叉验证）
//
// 语义真源：pkg/proto/token.go、pkg/proto/tunneladdr.go、本包 identity_store.go（基线 621fe0e）。
package main

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/zhaoyswd/homeway/pkg/proto"
)

// ---- 固定材料（字节稳定的确定性输入；hex 便于三方核对）----

var (
	masterA = mustHex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f") // 32B 顺序字节
	masterB = mustHex("f1e2d3c4b5a6978869584a3b2c1d0e0f00112233445566778899aabbccddeeff") // 32B
	peerID1 = mustHex("1111111111111111111111111111111111111111111111111111111111111111")
	peerID2 = mustHex("2222222222222222222222222222222222222222222222222222222222222222")
	peerID3 = mustHex("7bab5077e7ea7012143e4756372741f341ebb31e5cd4404a81b43324fd511849") // 烟囱出口真钥（2026-10-02 本地实例）
	secret1 = mustHex("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20")
	secret2 = mustHex("deadbeefcafebabe000000000000000000000000000000000000000000000001")
)

func mustHex(s string) []byte {
	b, err := hex.DecodeString(s)
	if err != nil {
		panic(err)
	}
	return b
}

func h32(b []byte) [32]byte {
	var out [32]byte
	if len(b) != 32 {
		panic("want 32B")
	}
	copy(out[:], b)
	return out
}

// ---- token 族 ----

type tokEndpoint struct {
	Addr  string `json:"addr"`
	Relay bool   `json:"relay"`
}

type tokenCase struct {
	Name     string       `json:"name"`
	Input    tokenInput   `json:"input"`
	Token    string       `json:"token"`
	Decoded  tokenDecoded `json:"decoded"`
	BodyB64  string       `json:"body_b64"` // base64url(裸载荷)——便于 Rust 侧分步断言（前缀/载荷/CRC）
	BodyHex  string       `json:"body_hex"`
	CrcHex   string       `json:"crc_hex"` // SHA256(body)[:4]
}

type tokenInput struct {
	PeerID    string        `json:"peer_id"`
	Secret    string        `json:"secret"`
	Endpoints []tokEndpoint `json:"endpoints"`
}

type tokenDecoded struct {
	PeerID    string        `json:"peer_id"`
	Secret    string        `json:"secret"`
	Endpoints []tokEndpoint `json:"endpoints"`
}

type tokenErrorCase struct {
	Name  string `json:"name"`
	Input string `json:"input"`
	// error = 三类哨兵错误之一（token.go:33-37）：corrupted / unsupported_version / malformed
	Error string `json:"error"`
}

func genTokenCases() (cases []tokenCase, errs []tokenErrorCase) {
	type spec struct {
		name  string
		peer  []byte
		sec   []byte
		eps   []proto.Endpoint
		note  string
	}
	specs := []spec{
		{"min-zero-endpoint", peerID1, secret1, nil, "合法下界：0 端点（epCount=0，载荷恰 69B）"},
		{"one-direct-localhost", peerID1, secret1, []proto.Endpoint{{"127.0.0.1:42641", false}}, "本地烟囱形态（单直连端点）"},
		{"three-mixed-with-domain", peerID2, secret2, []proto.Endpoint{
			{"192.168.3.12:41641", false}, {"home.example.com:41641", false}, {"198.51.100.212:41741", true},
		}, "直连+域名直连+中继混合（域名端点合法）"},
	}
	// 上界：255B 端点地址（encode 上限 len(e.Addr) <= 255，SplitHostPort 须可过）
	longHost := make([]byte, 0, 251)
	for i := 0; i < 251; i++ {
		longHost = append(longHost, byte('a'+i%26))
	}
	specs = append(specs, spec{"max-addr-255", peerID1, secret2,
		[]proto.Endpoint{{string(longHost) + ":443", false}}, "合法上界：端点地址恰 255 字节"})

	for _, sp := range specs {
		t := proto.Token{PeerID: h32(sp.peer), Secret: h32(sp.sec), Endpoints: sp.eps}
		tok, err := proto.EncodeToken(t)
		if err != nil {
			panic(fmt.Sprintf("%s: encode: %v", sp.name, err))
		}
		raw, _ := base64.RawURLEncoding.DecodeString(tok[len("hmw1"):])
		body, crc := raw[:len(raw)-4], raw[len(raw)-4:]
		sum := sha256.Sum256(body)
		if string(crc) != string(sum[:4]) {
			panic(sp.name + ": crc 不自洽")
		}
		d, err := proto.DecodeToken(tok)
		if err != nil {
			panic(fmt.Sprintf("%s: decode: %v", sp.name, err))
		}
		cases = append(cases, tokenCase{
			Name:     sp.name,
			Input:    tokenInput{hex.EncodeToString(sp.peer), hex.EncodeToString(sp.sec), convEps(sp.eps)},
			Token:    tok,
			Decoded:  tokenDecoded{hex.EncodeToString(d.PeerID[:]), hex.EncodeToString(d.Secret[:]), convEps(d.Endpoints)},
			BodyB64:  tok[len("hmw1"):],
			BodyHex:  hex.EncodeToString(body),
			CrcHex:   hex.EncodeToString(crc),
		})
	}

	// 错误向量：错误类别与 token.go 三哨兵一一对应；Rust 侧映射 enum 变体。
	good := proto.Token{PeerID: h32(peerID1), Secret: h32(secret1), Endpoints: []proto.Endpoint{{"127.0.0.1:42641", false}}}
	goodTok, _ := proto.EncodeToken(good)
	goodRaw, _ := base64.RawURLEncoding.DecodeString(goodTok[4:])
	flip := append([]byte(nil), goodRaw...)
	flip[len(flip)-1] ^= 0x01 // 破 CRC 末字节
	errs = append(errs,
		tokenErrorCase{"unsupported-version-hmw2", "hmw2" + goodTok[4:], "unsupported_version"},
		tokenErrorCase{"missing-prefix", goodTok[4:], "malformed"},
		tokenErrorCase{"crc-flip", "hmw1" + base64.RawURLEncoding.EncodeToString(flip), "corrupted"},
		tokenErrorCase{"truncated-body", "hmw1" + base64.RawURLEncoding.EncodeToString(goodRaw[:40]), "corrupted"},
		tokenErrorCase{"base64-padding-rejected", "hmw1" + goodTok[4:] + "=", "malformed"},
		tokenErrorCase{"whitespace-trimmed-ok-but-badcrc", " hmway1" + goodTok[4:], "malformed"}, // 前缀被空格破坏
	)
	// 载荷结构非法但 CRC 自洽：手工构载荷（epCount 声明 1、len=0）
	badBody := append([]byte(nil), peerID1...)
	badBody = append(badBody, secret1...)
	badBody = append(badBody, 1, 0) // epCount=1, addrLen=0 → malformed
	badSum := sha256.Sum256(badBody)
	badBody = append(badBody, badSum[:4]...)
	errs = append(errs, tokenErrorCase{"zero-addr-len-selfconsistent", "hmw1" + base64.RawURLEncoding.EncodeToString(badBody), "malformed"})
	// 尾部多一个字节（off != len(body)）但 CRC 覆盖全body → 结构 malformed
	trailBody := append([]byte(nil), peerID1...)
	trailBody = append(trailBody, secret1...)
	trailBody = append(trailBody, 0, 0x41) // epCount=0 + 多余 0x41
	trailSum := sha256.Sum256(trailBody)
	trailBody = append(trailBody, trailSum[:4]...)
	errs = append(errs, tokenErrorCase{"trailing-byte", "hmw1" + base64.RawURLEncoding.EncodeToString(trailBody), "malformed"})

	// 逐条校验错误类别（防向量本身写错类别）
	kind := func(err error) string {
		switch {
		case err == nil:
			return "ok"
		case errIs(err, proto.ErrCorrupted):
			return "corrupted"
		case errIs(err, proto.ErrUnsupportedVersion):
			return "unsupported_version"
		case errIs(err, proto.ErrMalformed):
			return "malformed"
		}
		return "?"
	}
	for _, e := range errs {
		_, derr := proto.DecodeToken(e.Input)
		if got := kind(derr); got != e.Error {
			panic(fmt.Sprintf("%s: 期望 %s 实得 %s（%v）", e.Name, e.Error, got, derr))
		}
	}
	return cases, errs
}

// errIs：哨兵比较（模板内不引 errors 包以保持依赖面最小——直接 ==，DecodeToken 的
// fmt.Errorf 包装经 %w 传链；这里用最小实现字符串前缀判定 + 精确哨兵判定）。
func errIs(err, target error) bool {
	for err != nil {
		if err == target {
			return true
		}
		u, ok := err.(interface{ Unwrap() error })
		if !ok {
			return false
		}
		err = u.Unwrap()
	}
	return false
}

func convEps(eps []proto.Endpoint) []tokEndpoint {
	out := make([]tokEndpoint, 0, len(eps))
	for _, e := range eps {
		out = append(out, tokEndpoint{e.Addr, e.Relay})
	}
	return out
}

// ---- 隧道地址族 ----

type addrCase struct {
	Name     string `json:"name"`
	Secret   string `json:"secret"`   // hex（token 的 Secret）
	Pubkey   string `json:"pubkey"`   // hex（设备身份公钥；函数面接受任意 32B）
	TunnelIP string `json:"tunnel_ip"` // 100.64.x.y（hw-tun）
	TunIP    string `json:"tun_ip"`    // 100.64.x.y（hw-app，含守卫再散列结果）
	Note     string `json:"note,omitempty"`
}

func genAddrCases() []addrCase {
	pairs := []struct {
		name string
		sec  []byte
		pub  []byte
		note string
	}{
		{"identity-pub-via-hkdf", secret1, mustHex("d5cae8cf000000000000000000000000000000000000000000000000000000ff"), "pub 为任意 32B（示例首 4B 取自烟囱客户端真钥）"},
		{"second-secret", secret2, peerID1, "secret 与 pubkey 均换"},
	}
	// 守卫命中样本：搜 (secret, pubkey) 使 hw-app 的 v == hw-tun 的 v ⇒ DeriveTunIP 走再散列循环。
	// v 域 1..65534，生日碰撞期望 ~65k 次尝试；上限 1<<20 兜底。
	guardFound := ""
	for i := 0; i < 1<<20 && guardFound == ""; i++ {
		pub := make([]byte, 32)
		pub[0] = byte(i >> 24)
		pub[1] = byte(i >> 16)
		pub[2] = byte(i >> 8)
		pub[3] = byte(i)
		va := hmacV(secret1, "hw-tun", pub, 0)
		vb := hmacV(secret1, "hw-app", pub, 2)
		if va == vb {
			guardFound = hex.EncodeToString(pub)
			pairs = append(pairs, struct {
				name, note          string
				sec, pub            []byte
			}{"guard-collision-rehash", secret1, pub, "hw-app 与 hw-tun 撞 v ⇒ DeriveTunIP 进再散列循环（hw-app.N 路径）"})
			break
		}
	}
	out := make([]addrCase, 0, len(pairs))
	for _, p := range pairs {
		tip := proto.DeriveTunnelIP(h32(p.sec), h32(p.pub))
		aip := proto.DeriveTunIP(h32(p.sec), h32(p.pub))
		out = append(out, addrCase{p.name, hex.EncodeToString(p.sec), hex.EncodeToString(p.pub), tip.String(), aip.String(), p.note})
	}
	return out
}

// hmacV：复刻 tunneladdr.go 的 v 计算（仅用于守卫搜索；向量值仍以 proto.Derive* 真源产出）。
func hmacV(secret []byte, label string, pubkey []byte, at int) uint16 {
	h := hmac.New(sha256.New, secret)
	h.Write([]byte(label))
	h.Write(pubkey)
	sum := h.Sum(nil)
	return uint16((uint32(sum[at])<<8 | uint32(sum[at+1])) % 65534)
}

// ---- identity 族 ----

type identityCase struct {
	Name       string `json:"name"`
	Master     string `json:"master"`      // hex 32B（<dir>/master.key 内容）
	PeerID     string `json:"peer_id"`     // hex 32B（token 的 PeerID）
	PrivateKey string `json:"private_key"` // hex 32B（HKDF 产出，**未钳位**——NewKey 为原样拷贝）
	PublicKey  string `json:"public_key"`  // hex 32B（X25519(clamp(priv), 9)——钳位在标量乘内部）
	ShortDev   string `json:"short_dev,omitempty"`
	Note       string `json:"note,omitempty"`
}

type devTagCase struct {
	Name    string `json:"name"`
	Master  string `json:"master"`
	DevTag  string `json:"dev_tag"`   // hex 8B
	ShortDev string `json:"short_dev"` // hex 4B（日志「dev=」形态）
}

func genIdentityCases() (ids []identityCase, tags []devTagCase) {
	specs := []struct {
		name  string
		master, peer []byte
		note string
	}{
		{"a-backend1", masterA, peerID1, "同 master 同后端 = 稳定身份（复用路径）"},
		{"a-backend2", masterA, peerID2, "同 master 换后端 = 换身份（跨出口不可关联）"},
		{"b-backend1", masterB, peerID1, "换 master（重置身份）换钥匙，后端看到新公钥"},
		{"smoke-exit-peer", masterA, peerID3, "后端 = 烟囱出口真钥"},
	}
	for _, sp := range specs {
		key, err := deriveKey(h32(sp.master), h32(sp.peer))
		if err != nil {
			panic(err)
		}
		pub := key.PublicKey()
		ids = append(ids, identityCase{
			Name: sp.name, Master: hex.EncodeToString(sp.master), PeerID: hex.EncodeToString(sp.peer),
			PrivateKey: hex.EncodeToString(key[:]), PublicKey: hex.EncodeToString(pub[:]), Note: sp.note,
		})
	}
	tags = append(tags,
		devTagCase{"master-a", hex.EncodeToString(masterA)},
		devTagCase{"master-b", hex.EncodeToString(masterB)},
	)
	for i := range tags {
		m := mustHex(tags[i].Master)
		tag, err := deriveDevTag(h32(m))
		if err != nil {
			panic(err)
		}
		tags[i].DevTag = hex.EncodeToString(tag[:])
		tags[i].ShortDev = hex.EncodeToString(tag[:4])
	}

	// 全路径交叉验证：固定 master 落盘 → LoadOrCreateIdentity 应产出与直调派生一致的钥匙。
	dir, err := os.MkdirTemp("", "vecgen-identity-*")
	if err != nil {
		panic(err)
	}
	defer os.RemoveAll(dir)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		panic(err)
	}
	if err := os.WriteFile(filepath.Join(dir, MasterKeyFile), masterA, 0o600); err != nil {
		panic(err)
	}
	// devtag 文件故意不写：走 loadOrCreateDevTag 的创建路径会引随机数 ⇒ 只验钥匙派生；
	//（devTag 持久化路径的确定性由 identity_store_test.go 的行为用例覆盖。）
	id, src, err := LoadOrCreateIdentity(dir, h32(peerID1))
	if err != nil || src != SourceReused {
		panic(fmt.Sprintf("store 路径异常：src=%v err=%v", src, err))
	}
	want, _ := deriveKey(h32(masterA), h32(peerID1))
	if id.PrivateKey() != want {
		panic("store 路径与直调派生不一致（向量不可信）")
	}
	return ids, tags
}

// ---- 输出 ----

func writeJSON(path string, v any) {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		panic(err)
	}
	b = append(b, '\n')
	if err := os.WriteFile(path, b, 0o644); err != nil {
		panic(err)
	}
	fmt.Printf("  %s（%d 字节）\n", path, len(b))
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "用法：go run vecgen_main.go <输出目录>")
		os.Exit(2)
	}
	out := os.Args[1]
	if err := os.MkdirAll(out, 0o755); err != nil {
		panic(err)
	}
	fmt.Println("==> 生成对照向量：")
	tokCases, tokErrs := genTokenCases()
	writeJSON(filepath.Join(out, "token.json"), map[string]any{
		"comment": "hmw1 编解码向量。布局：hmw1 ‖ base64url-raw( peerId(32) ‖ secret(32) ‖ epCount(1) ‖ [type(1)+len(1)+addr]* ‖ crc(4)=SHA256(body)[:4] )；type 0=direct 1=relay。语义真源 baseline pkg/proto/token.go。",
		"cases":   tokCases,
		"errors":  tokErrs,
	})
	writeJSON(filepath.Join(out, "tunnel_addr.json"), map[string]any{
		"comment": "隧道地址派生向量。tunnel_ip=HMAC-SHA256(secret,\"hw-tun\"‖pub) 取 sum[0:2] 映射 1..65534；tun_ip 同型用 \"hw-app\" 取 sum[2:4]，与 tunnel_ip 撞车时按 hw-app.2..8 再散列。语义真源 baseline pkg/proto/tunneladdr.go。",
		"cases":   genAddrCases(),
	})
	ids, tags := genIdentityCases()
	writeJSON(filepath.Join(out, "identity.json"), map[string]any{
		"comment": "设备身份派生向量。private_key=HKDF-SHA256(master, salt=nil, info=\"tier/dev-id/v1\"‖peerID, 32B)（未钳位）；public_key=X25519(钳位在乘内部, 基点9)。devTag=HKDF(master, info=\"tier/dev-tag/v1\", 8B)。已交叉验证：LoadOrCreateIdentity 全路径产出与直调一致。语义真源 baseline clientcore/internal/wtransport/identity_store.go。",
		"cases":   ids,
		"devtags": tags,
	})
	fmt.Println("==> 完成。")
}
