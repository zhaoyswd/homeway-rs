// vecgen_stun_sped_test.go — homeway-rs 对照向量生成器（R5 M20：STUN/SPED golden）。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/stun_sped/ 的**模板**，由 tools/gen-vectors.sh
// 拷进 baseline 克隆的 pkg/egress/ 再 `go test -run TestVecgenStunSped` 触发，
// **绝不 commit 进克隆**。放在 egress 包内 ⇒ STUN 走本包生产真源（StunRequest/
// ParseStunResponse），SPED 经 import 走 pkg/speedtest 生产真源（WriteControl/
// ReadFrameLoose——data 帧头为包内私有，样本手搓后经 ReadFrameLoose 验证形态）。
// 未设 HOMEWAY_VECGEN_OUT 时本用例 skip。
//
// 产出（$HOMEWAY_VECGEN_OUT/stun_sped.json，确定性——不含时间戳）：
//   stun.requests[]   StunRequest 四形态（software 空/对齐 0/1/2 字节——padding 边界）
//   stun.responses[]  手搓 Binding 应答（XOR/plain/双属性/截断）经 ParseStunResponse 的解析结果
//   sped.controls[]   WriteControl 三控制帧（request 带载荷/start/finish）
//   sped.datas[]      data 帧样本（1B/1400B/64KB len 边界；手搓头 + ReadFrameLoose 回读验证）
package egress

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"hash/crc32"
	"net/netip"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/zhaoyswd/homeway/pkg/speedtest"
)

func ssHex(b []byte) string { return hex.EncodeToString(b) }

func ssUnhex(s string) []byte {
	b, err := hex.DecodeString(s)
	if err != nil {
		panic(err)
	}
	return b
}

// 手搓 data 帧（对齐包内 writeData：magic/type=Data/seq LE/len LE/crc=zeroCRC(len)）。
func ssDataFrame(seq uint32, payloadLen int) []byte {
	out := make([]byte, 15+payloadLen)
	copy(out[0:4], "SPED")
	out[4] = byte(speedtest.TypeData)
	binary.LittleEndian.PutUint32(out[5:9], seq)
	binary.LittleEndian.PutUint16(out[9:11], uint16(payloadLen))
	binary.LittleEndian.PutUint32(out[11:15], crc32.ChecksumIEEE(make([]byte, payloadLen)))
	return out
}

// 手搓 Binding 应答（RFC 5389：type/magic/txID + 属性 TLV，4 字节对齐 padding）。
func ssStunResponse(txID [12]byte, attrs map[uint16][]byte, order []uint16) []byte {
	body := []byte{}
	for _, at := range order {
		val := attrs[at]
		body = binary.BigEndian.AppendUint16(body, at)
		body = binary.BigEndian.AppendUint16(body, uint16(len(val)))
		body = append(body, val...)
		for len(body)%4 != 0 {
			body = append(body, 0)
		}
	}
	out := make([]byte, 0, 20+len(body))
	out = binary.BigEndian.AppendUint16(out, 0x0101) // Binding Success
	out = binary.BigEndian.AppendUint16(out, uint16(len(body)))
	out = binary.BigEndian.AppendUint32(out, 0x2112A442)
	out = append(out, txID[:]...)
	out = append(out, body...)
	return out
}

// XOR-MAPPED-ADDRESS 属性值（family=1，端口/IP 按 cookie 异或）。
func ssXorAddr(ip [4]byte, port uint16) []byte {
	out := []byte{0x00, 0x01}
	p := port ^ uint16(0x2112)
	out = binary.BigEndian.AppendUint16(out, p)
	var a [4]byte
	binary.BigEndian.PutUint32(a[:], 0x2112A442)
	for i := 0; i < 4; i++ {
		out = append(out, ip[i]^a[i])
	}
	return out
}

// MAPPED-ADDRESS 属性值（明文）。
func ssPlainAddr(ip [4]byte, port uint16) []byte {
	out := []byte{0x00, 0x01}
	out = binary.BigEndian.AppendUint16(out, port)
	return append(out, ip[:]...)
}

func TestVecgenStunSped(t *testing.T) {
	out := os.Getenv("HOMEWAY_VECGEN_OUT")
	if out == "" {
		t.Skip("未设 HOMEWAY_VECGEN_OUT（模板被意外留在克隆里？）")
	}
	type reqCase struct {
		Name     string `json:"name"`
		Software string `json:"software"`
		TxID     string `json:"txid"`
		Wire     string `json:"wire"`
	}
	type respCase struct {
		Name    string `json:"name"`
		Wire    string `json:"wire"`
		TxID    string `json:"txid,omitempty"`
		Mapped  string `json:"mapped,omitempty"`
		WantErr string `json:"want_err,omitempty"`
	}
	type ctlCase struct {
		Name      string `json:"name"`
		FrameType uint8  `json:"frame_type"`
		Payload   string `json:"payload"`
		Wire      string `json:"wire"`
	}
	type dataCase struct {
		Name string `json:"name"`
		Seq  uint32 `json:"seq"`
		Len  int    `json:"len"`
		Wire string `json:"wire"`
	}

	tx := [12]byte{}
	for i := range tx {
		tx[i] = byte(i + 1)
	}
	tx2 := [12]byte{}
	for i := range tx2 {
		tx2[i] = byte(0xF0 + i) // 0xF0..0xFB（不溢出）
	}

	// ---- STUN 请求四形态（padding 边界：0/3/4/5 字节 software）----
	reqs := []reqCase{}
	for _, c := range []struct {
		name, sw string
	}{
		{"empty_sw", ""},
		{"sw3_pad1", "abc"},
		{"sw4_aligned", "abcd"},
		{"sw5_pad3", "abcde"},
	} {
		wire := StunRequest(tx, c.sw)
		reqs = append(reqs, reqCase{c.name, c.sw, ssHex(tx[:]), ssHex(wire)})
	}

	// ---- STUN 应答四样本（XOR/plain/双属性取 XOR/截断报错）----
	ip := [4]byte{203, 175, 12, 191}
	resps := []respCase{}
	{
		wire := ssStunResponse(tx, map[uint16][]byte{0x0020: ssXorAddr(ip, 29397)}, []uint16{0x0020})
		gotTx, mapped, err := ParseStunResponse(wire)
		if err != nil || gotTx != tx || mapped.String() != "203.175.12.191:29397" {
			t.Fatalf("xor 样本自检失败：%v %v %v", gotTx, mapped, err)
		}
		resps = append(resps, respCase{Name: "xor_only", Wire: ssHex(wire), TxID: ssHex(gotTx[:]), Mapped: mapped.String()})
	}
	{
		wire := ssStunResponse(tx, map[uint16][]byte{0x0001: ssPlainAddr(ip, 12345)}, []uint16{0x0001})
		_, mapped, err := ParseStunResponse(wire)
		if err != nil || mapped.String() != "203.175.12.191:12345" {
			t.Fatalf("plain 样本自检失败：%v %v", mapped, err)
		}
		resps = append(resps, respCase{Name: "plain_only", Wire: ssHex(wire), TxID: ssHex(tx[:]), Mapped: mapped.String()})
	}
	{
		// 双属性：XOR 优先于 plain
		wire := ssStunResponse(tx, map[uint16][]byte{0x0001: ssPlainAddr(ip, 1), 0x0020: ssXorAddr(ip, 2)}, []uint16{0x0001, 0x0020})
		_, mapped, err := ParseStunResponse(wire)
		if err != nil || mapped.String() != "203.175.12.191:2" {
			t.Fatalf("双属性自检失败：%v %v", mapped, err)
		}
		resps = append(resps, respCase{Name: "both_prefers_xor", Wire: ssHex(wire), TxID: ssHex(tx[:]), Mapped: mapped.String()})
	}
	{
		// 残缺：报文过短
		short := []byte{0x01, 0x01, 0x00}
		_, _, err := ParseStunResponse(short)
		if err == nil {
			t.Fatal("残缺样本应报错")
		}
		resps = append(resps, respCase{Name: "truncated", Wire: ssHex(short), WantErr: err.Error()})
	}
	{
		// txID 不匹配形态（ParseStunResponse 语义：返回解出的 txID 供调用方比对）
		wire := ssStunResponse(tx2, map[uint16][]byte{0x0020: ssXorAddr(ip, 80)}, []uint16{0x0020})
		gotTx, mapped, err := ParseStunResponse(wire)
		if err != nil || gotTx != tx2 {
			t.Fatalf("tx2 样本自检失败")
		}
		resps = append(resps, respCase{Name: "second_txid", Wire: ssHex(wire), TxID: ssHex(tx2[:]), Mapped: mapped.String()})
	}

	// ---- SPED 控制帧（WriteControl 生产真源）----
	ctls := []ctlCase{}
	for _, c := range []struct {
		name string
		ft   speedtest.FrameType
		pl   string
	}{
		{"request_json", speedtest.TypeRequest, `{"role":"recv","warmup_ms":2000,"window_ms":10000}`},
		{"start_empty", speedtest.TypeStart, ""},
		{"finish_empty", speedtest.TypeFinish, ""},
	} {
		var buf bytes.Buffer
		bw := bufio.NewWriter(&buf)
		if err := speedtest.WriteControl(bw, c.ft, []byte(c.pl)); err != nil {
			t.Fatalf("WriteControl：%v", err)
		}
		bw.Flush()
		wire := buf.Bytes()
		// 自检：生产解析器回读
		br := bufio.NewReader(bytes.NewReader(wire))
		ft, seq, n, pl, err := speedtest.ReadFrameLoose(br, make([]byte, 64*1024))
		if err != nil || uint8(ft) != uint8(c.ft) || seq != 0 || string(pl[:n]) != c.pl {
			t.Fatalf("控制帧回读自检失败：%v %v %v %v", ft, seq, n, err)
		}
		ctls = append(ctls, ctlCase{c.name, uint8(c.ft), c.pl, ssHex(wire)})
	}

	// ---- SPED data 帧三边界（1B/1400B/65535B；手搓头 + ReadFrameLoose 验证）----
	datas := []dataCase{}
	for _, c := range []struct {
		name string
		seq  uint32
		l    int
	}{
		{"len1", 1, 1},
		{"len1400", 0x0099_0001, 1400},
		{"len65535", 0xFFFF_FFFF, 65535},
	} {
		wire := ssDataFrame(c.seq, c.l)
		br := bufio.NewReader(bytes.NewReader(wire))
		ft, seq, n, _, err := speedtest.ReadFrameLoose(br, make([]byte, 128*1024))
		if err != nil || uint8(ft) != uint8(speedtest.TypeData) || seq != c.seq || n != c.l {
			t.Fatalf("data 帧回读自检失败：%v %v %v %v", ft, seq, n, err)
		}
		datas = append(datas, dataCase{c.name, c.seq, c.l, ssHex(wire)})
	}
	_ = time.Second
	_ = netip.Addr{}

	doc := map[string]any{
		"comment": "R5-M20 STUN/SPED golden（Go 生产真源产：StunRequest/ParseStunResponse/WriteControl/ReadFrameLoose；data 帧头手搓经 ReadFrameLoose 验证形态）",
		"stun":    map[string]any{"requests": reqs, "responses": resps},
		"sped":    map[string]any{"controls": ctls, "datas": datas},
	}
	blob, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(out, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(out, "stun_sped.json"), append(blob, '\n'), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Logf("写出 %s/stun_sped.json（%d 字节）", out, len(blob))
}
