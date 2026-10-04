// vecgen_term_test.go — homeway-rs R6 term 对照向量生成器（应答器 + 键/鼠标编码）。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/term/ 的**模板**，由 tools/gen-vectors.sh 拷进
// baseline 克隆的 pkg/term/vt/ 再 `go test -run TestVecgenTerm` 触发，**绝不 commit 进克隆**。
// 伴随文件 vecgen_term_aux.go（暴露 keys.go 之外的 cgo 键常量——test 文件不能用 cgo）
// 由同一脚本拷入/删除。未设 HOMEWAY_VECGEN_OUT 时本用例 skip。
//
// 产出（$HOMEWAY_VECGEN_OUT 下，内容确定性——无时间戳，重跑字节一致）：
//   term_responder.json  查询序列（DA/DSR/DECRQM/OSC 颜色/尺寸/标题）→ write_pty 应答字节
//   term_keyenc.json     模式态（DECCKM/kitty 五位/modifyOtherKeys）× KeyEvent → 转义序列
//   term_mouseenc.json   模式态（1000/1002/1003/1006/1005/9）× MouseEvent → 上报字节
//
// 语义真源 = libghostty-vt 的默认应答/编码行为（Go 绑定只装 write_pty sink、未装任何
// effects 回调 ⇒ 生产形态就是这些默认值）。Rust 侧（R6 6c）按本向量逐字节对拍。
package vt

import (
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

// ---- 通用 ----

type vecTermCollector struct{ chunks [][]byte }

func (c *vecTermCollector) sink(p []byte) {
	cp := make([]byte, len(p))
	copy(cp, p)
	c.chunks = append(c.chunks, cp)
}

func (c *vecTermCollector) all() string {
	var sb strings.Builder
	for _, ch := range c.chunks {
		sb.Write(ch)
	}
	return sb.String()
}

func vecHexStr(b []byte) string { return hex.EncodeToString(b) }

func vecWriteJSON(t *testing.T, name string, v any) {
	t.Helper()
	out := os.Getenv("HOMEWAY_VECGEN_OUT")
	if out == "" {
		return
	}
	data, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		t.Fatalf("marshal %s：%v", name, err)
	}
	data = append(data, '\n')
	if err := os.WriteFile(filepath.Join(out, name), data, 0o644); err != nil {
		t.Fatalf("write %s：%v", name, err)
	}
	t.Logf("%s：%d 字节", name, len(data))
}

// vecTermCase 一次被测会话：新建终端 → 喂 setup → 清采集 → 喂 query / 调 Encode*。
type vecTermCase struct {
	name   string
	setup  string // 预置序列（模式/光标/颜色；喂完重置采集）
	query  string // 被测查询序列（responder 族）
}

func vecRunResponder(t *testing.T, c vecTermCase, themed bool) (all string, chunks []string) {
	t.Helper()
	term, err := New(100, 32, 1000)
	if err != nil {
		t.Fatal(err)
	}
	defer term.Close()
	col := &vecTermCollector{}
	term.SetResponseSink(col.sink)
	if themed {
		term.SetDefaultColors([3]uint8{0x11, 0x22, 0x33}, [3]uint8{0xaa, 0xbb, 0xcc})
	}
	if c.setup != "" {
		term.Write([]byte(c.setup))
	}
	col.chunks = nil
	if c.query != "" {
		term.Write([]byte(c.query))
	}
	var buf []byte
	for _, ch := range col.chunks {
		buf = append(buf, ch...)
		chunks = append(chunks, vecHexStr(ch))
	}
	return vecHexStr(buf), chunks
}

// ---- ① 应答器 ----

type vecResponderCase struct {
	Name      string   `json:"name"`
	SetupHex  string   `json:"setup_hex,omitempty"`
	QueryHex  string   `json:"query_hex"`
	ResponseHex string `json:"response_hex"` // 空 = 不应答
	Chunks    []string `json:"chunks,omitempty"`
	Themed    bool     `json:"themed,omitempty"`
	Note      string   `json:"note,omitempty"`
}

func TestVecgenTermResponder(t *testing.T) {
	if os.Getenv("HOMEWAY_VECGEN_OUT") == "" {
		t.Skip("HOMEWAY_VECGEN_OUT 未设")
	}
	cases := []vecTermCase{
		// 设备属性族
		{name: "da1", query: "\x1b[c"},
		{name: "da1_zero", query: "\x1b[0c"},
		{name: "da2", query: "\x1b[>c"},
		{name: "da2_args", query: "\x1b[>0c"},
		{name: "da3", query: "\x1b[=c"},
		{name: "xtversion", query: "\x1b[?65c"},
		// DSR 族
		{name: "dsr_os", query: "\x1b[5n"},
		{name: "dsr_cpr", setup: "\x1b[3;7H", query: "\x1b[6n"},
		{name: "dsr_cpr_top_left", query: "\x1b[6n"},
		{name: "dsr_cpr_origin", setup: "\x1b[?6h\x1b[5;10H", query: "\x1b[6n"},
		{name: "dsr_cpr_origin_scrolled", setup: "\x1b[5;20r\x1b[?6h\x1b[3;4H", query: "\x1b[6n"},
		{name: "decxcpr", query: "\x1b[?6n"},
		// DECRQM 族（已知私有开/关、未知、ANSI、永久态）
		{name: "decrqm_priv_on", setup: "\x1b[?1000h", query: "\x1b[?1000$p"},
		{name: "decrqm_priv_off", setup: "\x1b[?1000l", query: "\x1b[?1000$p"},
		{name: "decrqm_priv_unknown", query: "\x1b[?9999$p"},
		{name: "decrqm_priv_117", query: "\x1b[?117$p"},
		{name: "decrqm_priv_7_on", query: "\x1b[?7$p"},
		{name: "decrqm_ansi_insert_on", setup: "\x1b[4h", query: "\x1b[4$p"},
		{name: "decrqm_ansi_insert_off", setup: "\x1b[4l", query: "\x1b[4$p"},
		{name: "decrqm_ansi_lnm", query: "\x1b[20$p"},
		{name: "decrqm_ansi_unknown", query: "\x1b[99$p"},
		// kitty 查询（默认/各档/pop 后）
		{name: "kitty_query_default", query: "\x1b[?u"},
		{name: "kitty_query_after_set1", setup: "\x1b[=1u", query: "\x1b[?u"},
		{name: "kitty_query_after_set31", setup: "\x1b[=31u", query: "\x1b[?u"},
		{name: "kitty_query_after_push_pop", setup: "\x1b[>15u\x1b[<1u", query: "\x1b[?u"},
		{name: "kitty_query_after_pop_all", setup: "\x1b[=15u\x1b[<1u", query: "\x1b[?u"},
		// OSC 颜色查询（默认值 = V-2 采值；上报后 = 主题应答）
		{name: "osc10_default", query: "\x1b]10;?\x07"},
		{name: "osc11_default", query: "\x1b]11;?\x07"},
		{name: "osc12_default", query: "\x1b]12;?\x07"},
		{name: "osc10_multi", query: "\x1b]10;?;?;?\x07"},
		{name: "osc10_11_multi", query: "\x1b]10;?;?\x07\x1b]11;?\x07"},
		{name: "osc4_mirror", setup: "\x1b]4;5;rgb:12/34/56\x07", query: "\x1b]4;5;?\x07"},
		{name: "osc4_unset", query: "\x1b]4;5;?\x07"},
		{name: "osc4_multi", query: "\x1b]4;1;?;2;?\x07"},
		// ST 终止形态（未设主题 = 不答；ST 采样）
		{name: "osc10_st_default", query: "\x1b]10;?\x1b\\"},
		{name: "osc4_st_mirror", setup: "\x1b]4;7;rgb:ab/cd/ef\x1b\\", query: "\x1b]4;7;?\x1b\\"},
		// 尺寸上报（New(100,32) + nominalCellPx=8：14t=4;H*rows;W*cols、16t=6;H;W、18t=8;rows;cols）
		{name: "csi14t", query: "\x1b[14t"},
		{name: "csi16t", query: "\x1b[16t"},
		{name: "csi18t", query: "\x1b[18t"},
		// 标题上报（DEC 21 默认关/开后）
		{name: "csi21t_off", query: "\x1b[21t"},
		{name: "csi21t_on", setup: "\x1b[?21h", query: "\x1b[21t"},
		// ENQ（未装 enquiry 回调 ⇒ 不答）
		{name: "enq", query: "\x05"},
		// modifyOtherKeys 查询（xterm XTQMODKEYS；herdr 补丁 0002 语义）
		{name: "mok_query_off", query: "\x1b[?4m"},
		{name: "mok_query_on", setup: "\x1b[>4;2m", query: "\x1b[?4m"},
		// 同批多查询（分片语义：一次 Write 多次 sink 调用）
		{name: "batch_da1_dsr", query: "\x1b[c\x1b[6n"},
	}
	out := make([]vecResponderCase, 0, len(cases)*2+4)
	for _, c := range cases {
		all, chunks := vecRunResponder(t, c, false)
		out = append(out, vecResponderCase{
			Name: c.name, SetupHex: vecHexStr([]byte(c.setup)), QueryHex: vecHexStr([]byte(c.query)),
			ResponseHex: all, Chunks: chunks,
		})
	}
	// 主题上报后的 OSC 10/11/12（design D4 唯一例外：应答值回传）+ ST 终止形态
	for _, q := range []struct {
		name  string
		query string
	}{
		{"osc10_theme", "\x1b]10;?\x07"},
		{"osc11_theme", "\x1b]11;?\x07"},
		{"osc12_theme", "\x1b]12;?\x07"},
		{"osc10_st_theme", "\x1b]10;?\x1b\\"},
		{"osc11_st_theme", "\x1b]11;?\x1b\\"},
	} {
		c := vecTermCase{name: q.name, query: q.query}
		all, chunks := vecRunResponder(t, c, true)
		out = append(out, vecResponderCase{
			Name: c.name, QueryHex: vecHexStr([]byte(c.query)),
			ResponseHex: all, Chunks: chunks, Themed: true,
		})
	}
	vecWriteJSON(t, "term_responder.json", map[string]any{"cases": out})

	// OSC 4 内置调色板全表（未 set 索引的应答值 = ghostty 默认 256 色表——Rust 内嵌同表用）。
	// 分批问（每批 16 索引，OSC 串不超长）；单独落 term_palette.json。
	var pal [256]string
	term, err := New(100, 32, 1000)
	if err != nil {
		t.Fatal(err)
	}
	defer term.Close()
	col := &vecTermCollector{}
	term.SetResponseSink(col.sink)
	for base := 0; base < 256; base += 16 {
		q := "\x1b]4;"
		for i := 0; i < 16; i++ {
			if i > 0 {
				q += ";"
			}
			q += itoa(base+i) + ";?"
		}
		col.chunks = nil
		term.Write([]byte(q + "\x07"))
		all := col.all()
		// 每索引应答形如 \x1b]4;<idx>;rgb:rrrr/gggg/bbbb\x07（BEL 终止）——逐段拆
		for _, seg := range strings.Split(all, "\x1b]") {
			if !strings.HasPrefix(seg, "4;") {
				continue
			}
			body := strings.TrimSuffix(seg, "\x07")
			parts := strings.SplitN(body, ";", 3)
			if len(parts) != 3 {
				t.Fatalf("OSC4 应答形态意外：%q", seg)
			}
			idx, err := strconv.Atoi(parts[1])
			if err != nil || idx < 0 || idx > 255 {
				t.Fatalf("OSC4 索引解析失败：%q", seg)
			}
			pal[idx] = parts[2]
		}
	}
	for i, v := range pal {
		if v == "" {
			t.Fatalf("OSC4 调色板索引 %d 未采到", i)
		}
	}
	vecWriteJSON(t, "term_palette.json", map[string]any{"osc4_unset": pal[:]})
}

func itoa(i int) string { return fmt.Sprintf("%d", i) }

// ---- ② 键编码 ----

type vecKeyEvent struct {
	Key        string `json:"key"`    // 键码表名（数值见 keys 表）
	Action     uint8  `json:"action"` // 0=release 1=press 2=repeat
	Mods       uint16 `json:"mods"`
	Text       string `json:"text,omitempty"`
	Unshifted  uint32 `json:"unshifted,omitempty"`
	Composing  bool   `json:"composing,omitempty"`
}

type vecKeyencCase struct {
	Mode   string      `json:"mode"` // 模式表名
	Name   string      `json:"name"`
	Event  vecKeyEvent `json:"event"`
	OutHex string      `json:"out_hex"` // 空 = 无输出（模式抑制）
}

type vecKeyencFile struct {
	Keys  map[string]uint16 `json:"keys"`
	Modes map[string]string `json:"modes"` // 模式名 → setup 序列 hex
	Cases []vecKeyencCase   `json:"cases"`
}

func TestVecgenTermKeyenc(t *testing.T) {
	if os.Getenv("HOMEWAY_VECGEN_OUT") == "" {
		t.Skip("HOMEWAY_VECGEN_OUT 未设")
	}

	// 键码表（keys.go 导出面 + vecgen_aux 注入面——数值即两仓共用 wire 键码表的 Go 侧真值）
	keys := map[string]uint16{}
	for _, k := range []struct {
		name string
		val  Key
	}{
		{"backquote", KeyBackquote}, {"minus", KeyMinus}, {"equal", KeyEqual},
		{"bracket_left", KeyBracketLeft}, {"bracket_right", KeyBracketRight},
		{"backslash", KeyBackslash}, {"semicolon", KeySemicolon}, {"quote", KeyQuote},
		{"comma", KeyComma}, {"period", KeyPeriod}, {"slash", KeySlash},
		{"digit0", KeyDigit0}, {"digit5", KeyDigit5},
		{"a", KeyA}, {"b", KeyB}, {"c", KeyC}, {"i", KeyI}, {"m", KeyM}, {"z", KeyZ},
		{"escape", KeyEscape}, {"enter", KeyEnter}, {"tab", KeyTab}, {"space", KeySpace},
		{"backspace", KeyBackspace}, {"delete", KeyDelete}, {"insert", KeyInsert},
		{"home", KeyHome}, {"end", KeyEnd}, {"page_up", KeyPageUp}, {"page_down", KeyPageDown},
		{"arrow_up", KeyArrowUp}, {"arrow_down", KeyArrowDown},
		{"arrow_left", KeyArrowLeft}, {"arrow_right", KeyArrowRight},
		{"f1", KeyF1}, {"f2", KeyF2}, {"f3", KeyF3}, {"f5", KeyF5}, {"f12", KeyF12},
		{"shift_left", VecKeyShiftLeft}, {"ctrl_left", VecKeyCtrlLeft},
		{"alt_left", VecKeyAltLeft}, {"super_left", VecKeySuperLeft},
		{"numpad0", VecKeyNumpad0}, {"numpad1", VecKeyNumpad1},
		{"numpad_enter", VecKeyNumpadEnter}, {"numpad_add", VecKeyNumpadAdd},
	} {
		keys[k.name] = uint16(k.val)
	}

	modes := []struct{ name, setup string }{
		{"plain", ""},
		{"decckm", "\x1b[?1h"},
		{"deckpam", "\x1b[?66h"},
		{"mok2", "\x1b[>4;2m"},
		{"kitty1", "\x1b[=1u"},
		{"kitty2", "\x1b[=2u"},
		{"kitty4", "\x1b[=4u"},
		{"kitty8", "\x1b[=8u"},
		{"kitty16", "\x1b[=16u"},
		{"kitty5", "\x1b[=5u"},
		{"kitty15", "\x1b[=15u"},
		{"kitty31", "\x1b[=31u"},
		{"kitty_131", "\x1b[=1u\x1b[=31u"},          // 重设为 31（set 全量替换语义）
		{"kitty_push_pop", "\x1b[>15u\x1b[<1u"},     // push 15 → pop 1 → 回默认 0
	}

	// 通用采样集：每个模式都跑（legacy/kitty 分叉全覆盖）
	type ev = vecKeyEvent
	common := []ev{
		{Key: "a", Action: 1, Text: "a"},
		{Key: "a", Action: 1, Mods: 2 /*ctrl*/, Text: "\x01"},
		{Key: "a", Action: 1, Mods: 1 /*shift*/, Text: "A"},
		{Key: "a", Action: 1, Mods: 4 /*alt*/, Text: "a"},
		{Key: "a", Action: 1, Mods: 6 /*ctrl+alt*/, Text: "\x01"},
		{Key: "enter", Action: 1, Text: "\r"},
		{Key: "tab", Action: 1, Text: "\t"},
		{Key: "backspace", Action: 1, Text: "\x7f"},
		{Key: "escape", Action: 1, Text: "\x1b"},
		{Key: "arrow_up", Action: 1},
		{Key: "arrow_left", Action: 1, Mods: 2},
		{Key: "arrow_right", Action: 1, Mods: 1},
		{Key: "f1", Action: 1},
		{Key: "f5", Action: 1, Mods: 2},
		{Key: "delete", Action: 1},
		{Key: "home", Action: 1},
		{Key: "page_up", Action: 1},
		{Key: "a", Action: 0 /*release*/, Text: "a"},
		{Key: "a", Action: 2 /*repeat*/, Text: "a"},
	}
	// plain 全量补充：文本键/控制字符映射/边界形态
	plainExtra := []ev{
		{Key: "space", Action: 1, Text: " "},
		{Key: "digit5", Action: 1, Text: "5"},
		{Key: "backquote", Action: 1, Text: "`"},
		{Key: "bracket_left", Action: 1, Text: "["},
		{Key: "z", Action: 1, Text: "z"},
		{Key: "z", Action: 1, Mods: 1, Text: "Z"},
		{Key: "c", Action: 1, Mods: 2, Text: "c"},
		{Key: "i", Action: 1, Mods: 2, Text: "i"},   // ctrl 表内除外项
		{Key: "m", Action: 1, Mods: 2, Text: "m"},   // 同上（CR 冲突）
		{Key: "bracket_left", Action: 1, Mods: 2, Text: "["}, // 同上（CSI 冲突）
		{Key: "space", Action: 1, Mods: 2, Text: " "},
		{Key: "backquote", Action: 1, Mods: 2, Text: "`"},
		{Key: "a", Action: 1, Text: "a", Composing: true},
		{Key: "a", Action: 1}, // 无 text 的字母键
		{Key: "escape", Action: 1},
	}
	// modifyOtherKeys=2 补充（CSI 27;mod;code~ 族）
	mokExtra := []ev{
		{Key: "a", Action: 1, Mods: 3 /*shift+ctrl*/, Text: "\x01"},
		{Key: "space", Action: 1, Mods: 1, Text: " "},
		{Key: "backquote", Action: 1, Mods: 2, Text: "`"},
		{Key: "comma", Action: 1, Mods: 2, Text: ","},
	}
	// kitty 专属补充（纯修饰键/不可打印 text/super）
	kittyExtra := []ev{
		{Key: "shift_left", Action: 1, Mods: 1},
		{Key: "ctrl_left", Action: 1, Mods: 2},
		{Key: "alt_left", Action: 1, Mods: 4},
		{Key: "a", Action: 1, Mods: 8 /*super*/, Text: "a"},
		{Key: "enter", Action: 1, Mods: 2, Text: "\r"},
		{Key: "enter", Action: 0, Text: "\r"},
		{Key: "backspace", Action: 1, Text: "xx"}, // IME 修正形态（utf8 非单段）
		{Key: "a", Action: 1, Text: "ä"},
		{Key: "numpad_enter", Action: 1},
		{Key: "numpad1", Action: 1, Text: "1"},
	}
	// deckpam 补充（小键盘分流）
	keypadExtra := []ev{
		{Key: "numpad0", Action: 1, Text: "0"},
		{Key: "numpad1", Action: 1, Text: "1"},
		{Key: "numpad_enter", Action: 1, Text: "\r"},
		{Key: "numpad_add", Action: 1, Text: "+"},
	}

	perModeExtra := map[string][]ev{
		"plain":  plainExtra,
		"mok2":   mokExtra,
		"deckpam": keypadExtra,
	}
	for _, m := range []string{"kitty1", "kitty2", "kitty8", "kitty16", "kitty15", "kitty31"} {
		perModeExtra[m] = kittyExtra
	}

	var out []vecKeyencCase
	for _, m := range modes {
		term, err := New(100, 32, 1000)
		if err != nil {
			t.Fatal(err)
		}
		if m.setup != "" {
			term.Write([]byte(m.setup))
		}
		events := append(append([]ev{}, common...), perModeExtra[m.name]...)
		for i, e := range events {
			got := term.EncodeKey(KeyEvent{
				Key: Key(keys[e.Key]), Action: KeyAction(e.Action), Mods: Mods(e.Mods),
				Text: e.Text, Composing: e.Composing,
			})
			ev := e
			out = append(out, vecKeyencCase{
				Mode: m.name, Name: fmt.Sprintf("%s#%02d", e.Key, i), Event: ev, OutHex: vecHexStr(got),
			})
		}
		term.Close()
	}
	// 模式序列也进 modes 表（hex）
	modesHex := map[string]string{}
	for _, m := range modes {
		modesHex[m.name] = vecHexStr([]byte(m.setup))
	}
	vecWriteJSON(t, "term_keyenc.json", vecKeyencFile{Keys: keys, Modes: modesHex, Cases: out})
}

// ---- ③ 鼠标编码 ----

type vecMouseEvent struct {
	Action uint8  `json:"action"` // 0=press 1=release 2=motion
	Button uint8  `json:"button"` // 1=left 2=right 3=middle 4=four 5=five
	Mods   uint16 `json:"mods"`
	X      uint16 `json:"x"`
	Y      uint16 `json:"y"`
}

type vecMouseCase struct {
	Mode   string        `json:"mode"`
	Name   string        `json:"name"`
	Event  vecMouseEvent `json:"event"`
	OutHex string        `json:"out_hex"`
}

func TestVecgenTermMouseenc(t *testing.T) {
	if os.Getenv("HOMEWAY_VECGEN_OUT") == "" {
		t.Skip("HOMEWAY_VECGEN_OUT 未设")
	}
	modes := []struct{ name, setup string }{
		{"moff", ""},
		{"m1000", "\x1b[?1000h"},
		{"m1002", "\x1b[?1002h"},
		{"m1003", "\x1b[?1003h"},
		{"m1000sgr", "\x1b[?1000h\x1b[?1006h"},
		{"m1002sgr", "\x1b[?1002h\x1b[?1006h"},
		{"m1003sgr", "\x1b[?1003h\x1b[?1006h"},
		{"m1000utf8", "\x1b[?1000h\x1b[?1005h"},
		{"m1002utf8", "\x1b[?1002h\x1b[?1005h"},
		{"mx10", "\x1b[?9h"},
	}
	events := []struct {
		name string
		ev   vecMouseEvent
	}{
		{"press_left", vecMouseEvent{Action: 0, Button: 1, X: 5, Y: 3}},
		{"press_right", vecMouseEvent{Action: 0, Button: 2, X: 5, Y: 3}},
		{"press_middle", vecMouseEvent{Action: 0, Button: 3, X: 5, Y: 3}},
		{"release_left", vecMouseEvent{Action: 1, Button: 1, X: 5, Y: 3}},
		{"release_right", vecMouseEvent{Action: 1, Button: 2, X: 5, Y: 3}},
		{"motion_left_held", vecMouseEvent{Action: 2, Button: 1, X: 6, Y: 4}},
		{"motion_no_button", vecMouseEvent{Action: 2, Button: 0, X: 6, Y: 4}},
		{"wheel_four", vecMouseEvent{Action: 0, Button: 4, X: 1, Y: 1}},
		{"wheel_five", vecMouseEvent{Action: 0, Button: 5, X: 1, Y: 1}},
		{"press_left_origin", vecMouseEvent{Action: 0, Button: 1, X: 0, Y: 0}},
		{"press_left_edge", vecMouseEvent{Action: 0, Button: 1, X: 222, Y: 222}},
		{"press_left_over", vecMouseEvent{Action: 0, Button: 1, X: 223, Y: 5}},
		{"press_left_shift", vecMouseEvent{Action: 0, Button: 1, Mods: 1, X: 5, Y: 3}},
		{"press_left_ctrl", vecMouseEvent{Action: 0, Button: 1, Mods: 2, X: 5, Y: 3}},
		{"press_left_ctrlshift", vecMouseEvent{Action: 0, Button: 1, Mods: 3, X: 5, Y: 3}},
		{"motion_left_ctrl", vecMouseEvent{Action: 2, Button: 1, Mods: 2, X: 7, Y: 2}},
	}
	var out []vecMouseCase
	for _, m := range modes {
		term, err := New(100, 32, 1000)
		if err != nil {
			t.Fatal(err)
		}
		if m.setup != "" {
			term.Write([]byte(m.setup))
		}
		for _, e := range events {
			got := term.EncodeMouse(MouseEvent{
				Action: MouseAction(e.ev.Action), Button: MouseButton(e.ev.Button),
				Mods: Mods(e.ev.Mods), X: e.ev.X, Y: e.ev.Y,
			})
			out = append(out, vecMouseCase{Mode: m.name, Name: e.name, Event: e.ev, OutHex: vecHexStr(got)})
		}
		term.Close()
	}
	modesHex := map[string]string{}
	for _, m := range modes {
		modesHex[m.name] = vecHexStr([]byte(m.setup))
	}
	vecWriteJSON(t, "term_mouseenc.json", map[string]any{"modes": modesHex, "cases": out})
}
