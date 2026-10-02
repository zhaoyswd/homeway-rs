// vecgen_vectors_test.go — homeway-rs 对照向量生成器（R0.4）。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/ 的**模板**，由 tools/gen-vectors.sh 拷进
// baseline 克隆的 clientcore/internal/wtransport/ 再 `go test -run TestVecgenVectors` 触发，
// **绝不 commit 进克隆**（克隆是 gitignore 的临时区）。放在包内 ⇒ 直调本包未导出函数
// （deriveKey/deriveDevTag），向量因此走的是**生产真源**而非旁路重写。
// 未设 HOMEWAY_VECGEN_OUT 时本用例 skip（若文件意外残留，正常测试零影响）。
//
// 产出（$HOMEWAY_VECGEN_OUT 下三个 JSON，内容确定性——不含时间戳，重跑字节一致，供 diff 门禁）：
//   token.json       hmw1 编解码 + 错误分类（corrupted/unsupported_version/malformed）
//   tunnel_addr.json DeriveTunnelIP / DeriveTunIP（含守卫命中样本：hw-app 与 hw-tun 撞车的再散列路径）
//   identity.json    master+peerID → WG 私钥/公钥；master → devTag（经 LoadOrCreateIdentity 全路径交叉验证）
//
// 语义真源：pkg/proto/token.go、pkg/proto/tunneladdr.go、本包 identity_store.go（基线 621fe0e）。
package wtransport

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/zhaoyswd/homeway/pkg/proto"
)

// ---- 固定材料（字节稳定的确定性输入；hex 便于三方核对）----

var (
	vecMasterA = vecHex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f") // 32B 顺序字节
	vecMasterB = vecHex("f1e2d3c4b5a6978869584a3b2c1d0e0f00112233445566778899aabbccddeeff") // 32B
	vecPeerID1 = vecHex("1111111111111111111111111111111111111111111111111111111111111111")
	vecPeerID2 = vecHex("2222222222222222222222222222222222222222222222222222222222222222")
	vecPeerID3 = vecHex("7bab5077e7ea7012143e4756372741f341ebb31e5cd4404a81b43324fd511849") // 烟囱出口真钥（2026-10-02 本地实例）
	vecSecret1 = vecHex("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20")
	vecSecret2 = vecHex("deadbeefcafebabe000000000000000000000000000000000000000000000001")
)

func vecHex(s string) []byte {
	b, err := hex.DecodeString(s)
	if err != nil {
		panic(err)
	}
	return b
}

func vecH32(b []byte) [32]byte {
	var out [32]byte
	if len(b) != 32 {
		panic("want 32B")
	}
	copy(out[:], b)
	return out
}

// ---- token 族 ----

type vecTokEndpoint struct {
	Addr  string `json:"addr"`
	Relay bool   `json:"relay"`
}

type vecTokenInput struct {
	PeerID    string           `json:"peer_id"`
	Secret    string           `json:"secret"`
	Endpoints []vecTokEndpoint `json:"endpoints"`
}

type vecTokenCase struct {
	Name    string `json:"name"`
	Input   vecTokenInput `json:"input"`
	Token   string `json:"token"` // 精确待解析串（whitespace 案例含首尾空白——DecodeToken 会 TrimSpace）
	Decoded vecTokenInput `json:"decoded"`
	BodyB64 string `json:"body_b64"` // base64url(裸载荷)——便于 Rust 侧分步断言（前缀/载荷/CRC）
	BodyHex string `json:"body_hex"`
	CrcHex  string `json:"crc_hex"` // SHA256(body)[:4]
	Note    string `json:"note,omitempty"`
}

type vecTokenErrorCase struct {
	Name  string `json:"name"`
	Input string `json:"input"`
	// error = 三哨兵之一（token.go:33-37）：corrupted / unsupported_version / malformed
	Error string `json:"error"`
}

func vecConvEps(eps []proto.Endpoint) []vecTokEndpoint {
	out := make([]vecTokEndpoint, 0, len(eps))
	for _, e := range eps {
		out = append(out, vecTokEndpoint{e.Addr, e.Relay})
	}
	return out
}

func genTokenCases(t *testing.T) (cases []vecTokenCase, errs []vecTokenErrorCase) {
	type spec struct {
		name string
		peer []byte
		sec  []byte
		eps  []proto.Endpoint
		note string
	}
	specs := []spec{
		{"min-zero-endpoint", vecPeerID1, vecSecret1, nil, "合法下界：0 端点（epCount=0，载荷恰 69B）"},
		{"one-direct-localhost", vecPeerID1, vecSecret1, []proto.Endpoint{{"127.0.0.1:42641", false}}, "本地烟囱形态（单直连端点）"},
		{"three-mixed-with-domain", vecPeerID2, vecSecret2, []proto.Endpoint{
			{"192.168.3.12:41641", false}, {"home.example.com:41641", false}, {"198.51.100.212:41741", true},
		}, "直连+域名直连+中继混合（域名端点合法）"},
		{"ipv6-bracket", vecPeerID1, vecSecret2, []proto.Endpoint{{"[::1]:53", false}}, "IPv6 括号形端点（SplitHostPort 括号分支）"},
	}
	// 上界：255B 端点地址（encode 上限 len(e.Addr) <= 255，SplitHostPort 须可过）
	longHost := make([]byte, 0, 251)
	for i := 0; i < 251; i++ {
		longHost = append(longHost, byte('a'+i%26))
	}
	specs = append(specs, spec{"max-addr-255", vecPeerID1, vecSecret2,
		[]proto.Endpoint{{string(longHost) + ":443", false}}, "合法上界：端点地址恰 255 字节"})

	for _, sp := range specs {
		tok := protoEncodeChecked(t, proto.Token{PeerID: vecH32(sp.peer), Secret: vecH32(sp.sec), Endpoints: sp.eps})
		raw, _ := base64.RawURLEncoding.DecodeString(tok[len("hmw1"):])
		body, crc := raw[:len(raw)-4], raw[len(raw)-4:]
		sum := sha256.Sum256(body)
		if string(crc) != string(sum[:4]) {
			t.Fatalf("%s: crc 不自洽", sp.name)
		}
		d, derr := proto.DecodeToken(tok)
		if derr != nil {
			t.Fatalf("%s: decode: %v", sp.name, derr)
		}
		cases = append(cases, vecTokenCase{
			Name:    sp.name,
			Input:   vecTokenInput{hex.EncodeToString(sp.peer), hex.EncodeToString(sp.sec), vecConvEps(sp.eps)},
			Token:   tok,
			Decoded: vecTokenInput{hex.EncodeToString(d.PeerID[:]), hex.EncodeToString(d.Secret[:]), vecConvEps(d.Endpoints)},
			BodyB64: tok[len("hmw1"):],
			BodyHex: hex.EncodeToString(body),
			CrcHex:  hex.EncodeToString(crc),
			Note:    sp.note,
		})
	}
	// TrimSpace 契约（正向）：首尾空白被剥离后照常解析（含 Unicode 全角空格 U+3000）
	trimmed := "\u3000  " + cases[0].Token + "\n\u3000"
	d, derr := proto.DecodeToken(trimmed)
	if derr != nil || len(d.Endpoints) != 0 || d.PeerID != vecH32(vecPeerID1) {
		t.Fatalf("TrimSpace 案例解析异常：err=%v", derr)
	}
	cases = append(cases, vecTokenCase{
		Name: "whitespace-trimmed", Input: cases[0].Input, Token: trimmed,
		Decoded: cases[0].Decoded, BodyB64: cases[0].BodyB64, BodyHex: cases[0].BodyHex, CrcHex: cases[0].CrcHex,
		Note: "DecodeToken 先 TrimSpace（Unicode White_Space，含 U+3000）——剥离后照常解析",
	})
	// 内嵌换行契约（正向）：Go base64 解码器跳过任意位置 \r/\n（终端折行；实测 nil）
	folded1 := cases[1].Token[:40] + "\n" + cases[1].Token[40:]
	folded2 := cases[1].Token[:40] + "\r\n" + cases[1].Token[40:]
	for _, f := range []string{folded1, folded2} {
		if _, err := proto.DecodeToken(f); err != nil {
			t.Fatalf("折行串应可解析：%v", err)
		}
	}
	cases = append(cases,
		vecTokenCase{Input: cases[1].Input, Token: folded1, Decoded: cases[1].Decoded,
			BodyB64: cases[1].BodyB64, BodyHex: cases[1].BodyHex, CrcHex: cases[1].CrcHex,
			Name: "embedded-lf-folded", Note: "base64 段内嵌 \\n——Go 解码器跳过（Rust 侧 decode 先剥离）"},
		vecTokenCase{Input: cases[1].Input, Token: folded2, Decoded: cases[1].Decoded,
			BodyB64: cases[1].BodyB64, BodyHex: cases[1].BodyHex, CrcHex: cases[1].CrcHex,
			Name: "embedded-crlf-folded", Note: "base64 段内嵌 \\r\\n——同上"},
	)

	// 错误向量：错误类别与 token.go 三哨兵一一对应；Rust 侧映射 enum 变体。
	good := proto.Token{PeerID: vecH32(vecPeerID1), Secret: vecH32(vecSecret1), Endpoints: []proto.Endpoint{{"127.0.0.1:42641", false}}}
	goodTok := protoEncodeChecked(t, good)
	goodRaw, _ := base64.RawURLEncoding.DecodeString(goodTok[4:])
	flip := append([]byte(nil), goodRaw...)
	flip[len(flip)-1] ^= 0x01 // 破 CRC 末字节
	badBody := append(append([]byte(nil), vecPeerID1...), vecSecret1...)
	badBody = append(badBody, 1, 0) // epCount=1, addrLen=0 → malformed
	badSum := sha256.Sum256(badBody)
	badBody = append(badBody, badSum[:4]...)
	trailBody := append(append([]byte(nil), vecPeerID1...), vecSecret1...)
	trailBody = append(trailBody, 0, 0x41) // epCount=0 + 多余 0x41
	trailSum := sha256.Sum256(trailBody)
	trailBody = append(trailBody, trailSum[:4]...)
	// epCount=2 只带 1 个端点（第二枚端点头越界 → malformed，token.go:143-145 分支）
	shortEpBody := append(append([]byte(nil), vecPeerID1...), vecSecret1...)
	shortEpBody = append(shortEpBody, 2, 0, 15, '1', '2', '7', '.', '0', '.', '0', '.', '1', ':', '4', '2', '6', '0')
	shortEpSum := sha256.Sum256(shortEpBody)
	shortEpBody = append(shortEpBody, shortEpSum[:4]...)
	// addrLen 声明 200、实际只 10 字节（token.go:149-151 分支）
	overflowBody := append(append([]byte(nil), vecPeerID1...), vecSecret1...)
	overflowBody = append(overflowBody, 1, 200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)
	overflowSum := sha256.Sum256(overflowBody)
	overflowBody = append(overflowBody, overflowSum[:4]...)
	// 端点结构合法但 host:port 校验不过（token.go:154-156 分支：SplitHostPort 拒 "noport"）
	noportBody := append(append([]byte(nil), vecPeerID1...), vecSecret1...)
	noportBody = append(noportBody, 1, 6, 'n', 'o', 'p', 'o', 'r', 't')
	noportSum := sha256.Sum256(noportBody)
	noportBody = append(noportBody, noportSum[:4]...)
	errs = append(errs,
		vecTokenErrorCase{"unsupported-version-hmw2", "hmw2" + goodTok[4:], "unsupported_version"},
		vecTokenErrorCase{"missing-prefix", goodTok[4:], "malformed"},
		vecTokenErrorCase{"crc-flip", "hmw1" + base64.RawURLEncoding.EncodeToString(flip), "corrupted"},
		vecTokenErrorCase{"truncated-body", "hmw1" + base64.RawURLEncoding.EncodeToString(goodRaw[:40]), "corrupted"},
		vecTokenErrorCase{"base64-padding-rejected", "hmw1" + goodTok[4:] + "=", "malformed"}, // FIX-89：严格 RawURLEncoding，'=' 尾缀不容忍
		vecTokenErrorCase{"zero-addr-len-selfconsistent", "hmw1" + base64.RawURLEncoding.EncodeToString(badBody), "malformed"},
		vecTokenErrorCase{"trailing-byte", "hmw1" + base64.RawURLEncoding.EncodeToString(trailBody), "malformed"},
		vecTokenErrorCase{"ep-head-insufficient", "hmw1" + base64.RawURLEncoding.EncodeToString(shortEpBody), "malformed"},
		vecTokenErrorCase{"addr-len-overflow", "hmw1" + base64.RawURLEncoding.EncodeToString(overflowBody), "malformed"},
		vecTokenErrorCase{"hostport-reject-noport", "hmw1" + base64.RawURLEncoding.EncodeToString(noportBody), "malformed"},
	)
	// 逐条校验错误类别（防向量本身写错类别）
	kind := func(err error) string {
		switch {
		case errors.Is(err, proto.ErrCorrupted):
			return "corrupted"
		case errors.Is(err, proto.ErrUnsupportedVersion):
			return "unsupported_version"
		case errors.Is(err, proto.ErrMalformed):
			return "malformed"
		}
		return "?"
	}
	for _, e := range errs {
		_, derr := proto.DecodeToken(e.Input)
		if got := kind(derr); got != e.Error {
			t.Fatalf("%s: 期望 %s 实得 %s（%v）", e.Name, e.Error, got, derr)
		}
	}
	return cases, errs
}

// EncodeTokenChecked：encode 失败即终止（向量生成里 encode 输入都应合法）。
func protoEncodeChecked(t *testing.T, tok proto.Token) string {
	t.Helper()
	s, err := proto.EncodeToken(tok)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	return s
}

// ---- 隧道地址族 ----

type vecAddrCase struct {
	Name     string `json:"name"`
	Secret   string `json:"secret"`    // hex（token 的 Secret）
	Pubkey   string `json:"pubkey"`    // hex（设备身份公钥；函数面接受任意 32B）
	TunnelIP string `json:"tunnel_ip"` // 100.64.x.y（hw-tun）
	TunIP    string `json:"tun_ip"`    // 100.64.x.y（hw-app，含守卫再散列结果）
	Note     string `json:"note,omitempty"`
}

// vecHmacV：复刻 tunneladdr.go 的 v 计算（仅用于守卫命中搜索；向量值仍以 proto.Derive* 真源产出）。
func vecHmacV(secret []byte, label string, pubkey []byte, at int) uint16 {
	h := hmac.New(sha256.New, secret)
	h.Write([]byte(label))
	h.Write(pubkey)
	sum := h.Sum(nil)
	return uint16((uint32(sum[at])<<8 | uint32(sum[at+1])) % 65534)
}

func genAddrCases(t *testing.T) []vecAddrCase {
	pairs := []struct {
		name string
		sec  []byte
		pub  []byte
		note string
	}{
		{"secret1-pub-mixed", vecSecret1, vecHex("d5cae8cf000000000000000000000000000000000000000000000000000000ff"), "pub 为任意 32B（首 4B 取自烟囱客户端真钥）"},
		{"secret2-peer1", vecSecret2, vecPeerID1, "secret 与 pubkey 均换"},
	}
	// 守卫命中样本：搜 (secret, pubkey) 使 hw-app 的 v == hw-tun 的 v ⇒ DeriveTunIP 进再散列循环。
	// v 域 1..65534，碰撞期望 ~65k 次尝试；上限 1<<20 兜底（miss 概率 ~e^-15）。
	// 搜不到 = 向量集失去守卫路径覆盖 ⇒ 直接判失败（不静默降级）。
	guardFound := false
	for i := 0; i < 1<<20 && !guardFound; i++ {
		pub := make([]byte, 32)
		pub[0], pub[1], pub[2], pub[3] = byte(i>>24), byte(i>>16), byte(i>>8), byte(i)
		if vecHmacV(vecSecret1, "hw-tun", pub, 0) == vecHmacV(vecSecret1, "hw-app", pub, 2) {
			pairs = append(pairs, struct {
				name string
				sec  []byte
				pub  []byte
				note string
			}{"guard-collision-rehash", vecSecret1, pub, "hw-app 与 hw-tun 撞 v ⇒ DeriveTunIP 走 hw-app.N 再散列（守卫路径）"})
			guardFound = true
		}
	}
	if !guardFound {
		t.Fatal("守卫命中样本未找到（1<<20 次内未撞 v）——向量集不完整，拒绝生成")
	}
	out := make([]vecAddrCase, 0, len(pairs))
	for _, p := range pairs {
		tip := proto.DeriveTunnelIP(vecH32(p.sec), vecH32(p.pub))
		aip := proto.DeriveTunIP(vecH32(p.sec), vecH32(p.pub))
		out = append(out, vecAddrCase{p.name, hex.EncodeToString(p.sec), hex.EncodeToString(p.pub), tip.String(), aip.String(), p.note})
	}
	return out
}

// ---- identity 族 ----

type vecIdentityCase struct {
	Name       string `json:"name"`
	Master     string `json:"master"`      // hex 32B（<dir>/master.key 内容）
	PeerID     string `json:"peer_id"`     // hex 32B（token 的 PeerID）
	PrivateKey string `json:"private_key"` // hex 32B（HKDF 产出，未钳位——wgtypes.NewKey 为原样拷贝）
	PublicKey  string `json:"public_key"`  // hex 32B（curve25519.ScalarBaseMult——钳位在标量乘内部）
	Note       string `json:"note,omitempty"`
}

type vecDevTagCase struct {
	Name     string `json:"name"`
	Master   string `json:"master"`
	DevTag   string `json:"dev_tag"`   // hex 8B
	ShortDev string `json:"short_dev"` // hex 4B（日志「dev=」形态，shortTag）
}

func genIdentityCases(t *testing.T) (ids []vecIdentityCase, tags []vecDevTagCase) {
	specs := []struct {
		name         string
		master, peer []byte
		note         string
	}{
		{"a-backend1", vecMasterA, vecPeerID1, "同 master 同后端 = 稳定身份（复用路径）"},
		{"a-backend2", vecMasterA, vecPeerID2, "同 master 换后端 = 换身份（跨出口不可关联）"},
		{"b-backend1", vecMasterB, vecPeerID1, "换 master（重置身份）换钥匙，后端看到新公钥"},
		{"smoke-exit-peer", vecMasterA, vecPeerID3, "后端 = 烟囱出口真钥"},
	}
	for _, sp := range specs {
		key, err := deriveKey(vecH32(sp.master), vecH32(sp.peer))
		if err != nil {
			t.Fatalf("%s: %v", sp.name, err)
		}
		pub := key.PublicKey()
		ids = append(ids, vecIdentityCase{
			Name: sp.name, Master: hex.EncodeToString(sp.master), PeerID: hex.EncodeToString(sp.peer),
			PrivateKey: hex.EncodeToString(key[:]), PublicKey: hex.EncodeToString(pub[:]), Note: sp.note,
		})
	}
	for _, m := range [][]byte{vecMasterA, vecMasterB} {
		tag, err := deriveDevTag(vecH32(m))
		if err != nil {
			t.Fatal(err)
		}
		tags = append(tags, vecDevTagCase{
			Name: "master-" + hex.EncodeToString(m[:1]), Master: hex.EncodeToString(m),
			DevTag: hex.EncodeToString(tag[:]), ShortDev: hex.EncodeToString(tag[:4]),
		})
	}

	// 全路径交叉验证：固定 master 落盘 → LoadOrCreateIdentity 应产出与直调派生一致的钥匙。
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, MasterKeyFile), vecMasterA, 0o600); err != nil {
		t.Fatal(err)
	}
	// devtag 文件故意不写：走 loadOrCreateDevTag 的创建路径会引随机数 ⇒ 只验钥匙派生；
	// devTag 持久化路径的确定性由 identity_store_test.go 的行为用例覆盖。
	id, src, err := LoadOrCreateIdentity(dir, vecH32(vecPeerID1))
	if err != nil || src != SourceReused {
		t.Fatalf("store 路径异常：src=%v err=%v", src, err)
	}
	want, _ := deriveKey(vecH32(vecMasterA), vecH32(vecPeerID1))
	if id.PrivateKey() != want {
		t.Fatal("store 路径与直调派生不一致（向量不可信）")
	}
	return ids, tags
}

// ---- psk 族（R1 技术评审 S1：PSK 在 WG 握手热路径，派生错 = AEAD tag 失败，须向量钉死）----

type vecPskCase struct {
	Name   string `json:"name"`
	Secret string `json:"secret"` // hex 32B（token 的 Secret）
	Psk    string `json:"psk"`    // hex 32B = HKDF-SHA256(ikm=secret, salt=nil, info="homeway/wg-psk", 32)
	Note   string `json:"note,omitempty"`
}

func genPskCases(t *testing.T) []vecPskCase {
	specs := []struct {
		name string
		sec  []byte
		note string
	}{
		{"secret1-sequential", vecSecret1, "顺序字节 secret（与隧道地址族共用材料，便于交叉核对）"},
		{"secret2-patterned", vecSecret2, "图案 secret"},
		{"all-zero-not-applicable-guard", nil, ""}, // 占位剔除：全零 secret 不合法（token 层已拒），不产向量
	}
	out := make([]vecPskCase, 0, len(specs))
	for _, sp := range specs {
		if sp.sec == nil {
			continue
		}
		psk := proto.DerivePSK(vecH32(sp.sec))
		out = append(out, vecPskCase{sp.name, hex.EncodeToString(sp.sec), hex.EncodeToString(psk[:]), sp.note})
	}
	if len(out) == 0 {
		t.Fatal("psk 向量为空")
	}
	return out
}

// ---- 输出 ----

func vecWriteJSON(t *testing.T, path string, v any) {
	t.Helper()
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	b = append(b, '\n')
	if err := os.WriteFile(path, b, 0o644); err != nil {
		t.Fatal(err)
	}
	fmt.Printf("  %s（%d 字节）\n", path, len(b))
}

func TestVecgenVectors(t *testing.T) {
	out := os.Getenv("HOMEWAY_VECGEN_OUT")
	if out == "" {
		t.Skip("向量生成模式未开（需 HOMEWAY_VECGEN_OUT=<目录>；homeway-rs tools/gen-vectors.sh 调用）")
	}
	if err := os.MkdirAll(out, 0o755); err != nil {
		t.Fatal(err)
	}
	fmt.Println("==> 生成对照向量：")
	tokCases, tokErrs := genTokenCases(t)
	vecWriteJSON(t, filepath.Join(out, "token.json"), map[string]any{
		"comment": "hmw1 编解码向量。布局：hmw1 ‖ base64url-raw( peerId(32) ‖ secret(32) ‖ epCount(1) ‖ [type(1)+len(1)+addr]* ‖ crc(4)=SHA256(body)[:4] )；type 0=direct 1=relay；DecodeToken 先 TrimSpace、base64 解码跳过内嵌 \\r\\n。语义真源 baseline pkg/proto/token.go。",
		"sentinels": map[string]string{
			"comment":              "三类哨兵错误的 Go Error() 原文（Display 文案经 NAPI 直达 App，Rust 侧须逐字对齐前缀段；malformed 的 reason 段按打点各异、Rust 侧仅对齐「homeway/token: 格式非法: 」前缀）",
			"corrupted":            proto.ErrCorrupted.Error(),
			"unsupported_version":  proto.ErrUnsupportedVersion.Error(),
			"unsupported_wrapped":  fmt.Errorf("%w: hmw2", proto.ErrUnsupportedVersion).Error(),
			"malformed":            proto.ErrMalformed.Error(),
			"malformed_no_prefix":  fmt.Errorf("%w: 缺少 %s 前缀", proto.ErrMalformed, "hmw1").Error(),
		},
		"cases":  tokCases,
		"errors": tokErrs,
	})
	vecWriteJSON(t, filepath.Join(out, "tunnel_addr.json"), map[string]any{
		"comment": "隧道地址派生向量。tunnel_ip=HMAC-SHA256(secret,\"hw-tun\"‖pub) 取 sum[0:2] 映射 v∈[1,65534]；tun_ip 同型用 \"hw-app\" 取 sum[2:4]，与 tunnel_ip 撞车时按 hw-app.2..8 标签再散列。地址恒 100.64.(v>>8).v。语义真源 baseline pkg/proto/tunneladdr.go。",
		"cases":   genAddrCases(t),
	})
	ids, tags := genIdentityCases(t)
	vecWriteJSON(t, filepath.Join(out, "identity.json"), map[string]any{
		"comment": "设备身份派生向量。private_key=HKDF-SHA256(master, salt=nil, info=\"tier/dev-id/v1\"‖peerID, 32B)（未钳位）；public_key=curve25519.ScalarBaseMult（钳位在标量乘内部，Rust 侧 x25519 同义）；devTag=HKDF(master, info=\"tier/dev-tag/v1\", 8B)。已交叉验证：LoadOrCreateIdentity 全路径产出与直调派生一致。语义真源 baseline clientcore/internal/wtransport/identity_store.go。",
		"cases":   ids,
		"devtags": tags,
	})
	vecWriteJSON(t, filepath.Join(out, "psk.json"), map[string]any{
		"comment": "WG PSK 派生向量。psk=HKDF-SHA256(ikm=token.secret, salt=nil, info=\"homeway/wg-psk\", 32B)；客户端 peer 配置与出口 peer 登记共用本派生（握手 psk2 混入）。语义真源 baseline pkg/proto/psk.go。",
		"cases":   genPskCases(t),
	})
	fmt.Println("==> 完成。")
}
