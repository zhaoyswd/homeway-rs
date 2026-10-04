//go:build !windows && (darwin || linux) && (amd64 || arm64) && cgo

// vecgen_surface_test.go — homeway-rs R6 6e surface 体编码对照向量生成器。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/term/ 的**模板**，由 tools/gen-vectors.sh 拷进
// baseline 克隆的 pkg/term/ 再 `go test -run TestVecgenSurface` 触发，**绝不 commit 进克隆**。
// 未设 HOMEWAY_VECGEN_OUT 时本用例 skip。
//
// 产出（$HOMEWAY_VECGEN_OUT/surface_codec.json，内容确定性——重跑字节一致）：
//   cellcodec 族：手造 Row/Cell（三标记流的全部迁移形态 + varint 多字节 + 颜色/属性/占位）
//                 → vt.EncodeGrid/EncodeRows 逐字节；负例 = 篡改字节 → Decode 报错。
//   体族：        SNAPSHOT/DIFF/FETCH-ROWS 请求与应答（encSnapshotBody 等未导出面——本文件
//                 在包内直调）逐字节 + 字段表。
//   分片族：      fragmentPayload 的空/单片形态（多片算术留给 Rust 单测——60KiB 边界进向量
//                 会让 JSON 膨胀 120KB+，而分片切片本身无方言差）。
//
// 这是「逐字节」的格式锚：与仿真器无关（EncodeGrid 只吃 Row 值），
// golden .bin 夹具另从真实会话字节产（surface-golden/，Rust 侧另有对拍测试）。
package term

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/zhaoyswd/homeway/pkg/term/vt"
)

// ---- JSON 形状 ----

type vecCell struct {
	Sym  string   `json:"sym,omitempty"`
	Width uint8   `json:"width,omitempty"`
	Skip bool     `json:"skip,omitempty"`
	FG   []byte   `json:"fg,omitempty"` // 空/缺 = none；[1,idx] = palette；[2,r,g,b] = rgb
	BG   []byte   `json:"bg,omitempty"`
	Attr *uint16  `json:"attr,omitempty"`
}

type vecRowIn struct {
	Y     uint16    `json:"y"`
	Cells []vecCell `json:"cells"`
}

type vecCellCase struct {
	Name     string     `json:"name"`
	Cols     uint16     `json:"cols"`
	RowsIn   []vecRowIn `json:"rows_in"`
	GridHex  string     `json:"grid_hex"`
	RowsHex  string     `json:"rows_hex"` // vt.EncodeRows（行序列，无头版）
	Note     string     `json:"note,omitempty"`
}

type vecDecodeBad struct {
	Name    string `json:"name"`
	InHex   string `json:"in_hex"`   // DecodeGrid 输入
	Count   int    `json:"count"`    // DecodeRows 用（0 = 不测 rows 面）
	Cols    int    `json:"cols"`
	Note    string `json:"note,omitempty"`
}

type vecBodyCase struct {
	Name   string `json:"name"`
	BodyHex string `json:"body_hex"`
	Note   string `json:"note,omitempty"`
}

type vecFragCase struct {
	Name  string   `json:"name"`
	InHex string   `json:"in_hex"`
	Frags []vecFrag `json:"frags"`
	Note  string   `json:"note,omitempty"`
}

type vecFrag struct {
	Flags uint8  `json:"flags"`
	Hex   string `json:"hex"`
}

type vecSurfaceCodecFile struct {
	Cells      []vecCellCase  `json:"cells"`
	CellsBad   []vecDecodeBad `json:"cells_bad"`
	Snapshot   []vecBodyCase  `json:"snapshot"`
	Diff       []vecBodyCase  `json:"diff"`
	FetchReq   []vecBodyCase  `json:"fetch_req"`
	FetchReply []vecBodyCase  `json:"fetch_reply"`
	Fragment   []vecFragCase  `json:"fragment"`
}

// ---- cell 构造小件 ----

func vecC(sym string, fg, bg []byte, attr uint16) vt.Cell {
	c := vt.Cell{Symbol: sym, Width: 1, FG: vecColor(fg), BG: vecColor(bg), Attr: attr}
	if sym == "" {
		c.Width = 0
	}
	return c
}

func vecColor(k []byte) vt.Color {
	if len(k) >= 2 && k[0] == 1 {
		return vt.Color{Kind: vt.ColorPalette, Index: k[1]}
	}
	if len(k) >= 4 && k[0] == 2 {
		return vt.Color{Kind: vt.ColorRGB, R: k[1], G: k[2], B: k[3]}
	}
	return vt.Color{}
}

func vecBlank(n int) []vt.Cell {
	out := make([]vt.Cell, n)
	for i := range out {
		out[i] = vt.Cell{Width: 1}
	}
	return out
}

func vecRep(n int, c vt.Cell) []vt.Cell {
	out := make([]vt.Cell, n)
	for i := range out {
		out[i] = c
	}
	return out
}

func TestVecgenSurface(t *testing.T) {
	if os.Getenv("HOMEWAY_VECGEN_OUT") == "" {
		t.Skip("HOMEWAY_VECGEN_OUT 未设")
	}

	box := vecC("─", []byte{1, 4}, nil, 0) // 调色板 4 前景的框线（repeatRun 形态）

	cases := []struct {
		name string
		cols uint16
		rows []vt.Row
		note string
	}{
		{"blank_row", 10, []vt.Row{{Y: 0, Cells: vecBlank(10)}}, "全空白行 → 单 blankRun"},
		{"blank_127", 127, []vt.Row{{Y: 0, Cells: vecBlank(127)}}, "varint 单字节上限"},
		{"blank_128", 128, []vt.Row{{Y: 0, Cells: vecBlank(128)}}, "varint 双字节起"},
		{"blank_300", 300, []vt.Row{{Y: 0, Cells: vecBlank(300)}}, "varint 多字节"},
		{"repeat_box", 10, []vt.Row{{Y: 0, Cells: vecRep(10, box)}}, "全重复行 → 首格 cell + repeatRun"},
		{"cell_blank_cell", 6, []vt.Row{{Y: 0, Cells: append(append(
			[]vt.Cell{vecC("A", nil, nil, 0)}, vecBlank(3)...), vecC("B", nil, nil, 0))}},
			"cell→blank→cell：flush 顺序"},
		{"repeat_blank_repeat", 8, []vt.Row{{Y: 0, Cells: append(vecRep(3, box), append(
			vecBlank(2), vecRep(3, box)...)...)}}, "repeat→blank→repeat：两个 repeatRun 被空白隔开"},
		{"blank_repeat_blank", 7, []vt.Row{{Y: 0, Cells: append(vecBlank(2), append(
			vecRep(3, box), vecBlank(2)...)...)}}, "blank→repeat→blank：首 repeat 前无 prev 格"},
		{"wide_tail", 4, []vt.Row{{Y: 3, Cells: []vt.Cell{
			{Symbol: "宽", Width: 2}, {Symbol: "", Width: 0, Skip: true},
			vecC("A", nil, nil, 0), {Width: 1}}}},
			"宽字符 + 占位格（skipBit）+ 尾部空白"},
		{"colors", 4, []vt.Row{{Y: 0, Cells: []vt.Cell{
			vecC("a", nil, nil, 0),
			vecC("b", []byte{1, 196}, []byte{1, 17}, 0),
			vecC("c", []byte{2, 10, 20, 30}, []byte{2, 1, 2, 3}, 0),
			vecC("d", nil, []byte{1, 0}, 0),
		}}}, "none/palette/rgb 三 kind 的 fg×bg 组合"},
		{"attr_bits", 9, []vt.Row{{Y: 0, Cells: []vt.Cell{
			vecC("x", nil, nil, 1<<0), vecC("x", nil, nil, 1<<1), vecC("x", nil, nil, 1<<2),
			vecC("x", nil, nil, 1<<3), vecC("x", nil, nil, 1<<4), vecC("x", nil, nil, 1<<5),
			vecC("x", nil, nil, 1<<6), vecC("x", nil, nil, 1<<7), vecC("x", nil, nil, 1<<8|2),
		}}}, "attr 各位 + 下划线样式位（bit8..11）"},
		{"styled_space", 3, []vt.Row{{Y: 0, Cells: []vt.Cell{
			vecC("", nil, []byte{2, 9, 9, 9}, 1<<4), {Width: 1}, vecC("Z", nil, nil, 0),
		}}}, "带样式空格（sym 空但 attr/bg 非零 → 非 blank，编完整格）+ 中间无样式空白格"},
		{"multi_row_nonseq", 4, []vt.Row{
			{Y: 5, Cells: vecRep(4, box)},
			{Y: 2, Cells: vecBlank(4)},
			{Y: 9, Cells: []vt.Cell{vecC("Q", nil, nil, 0), {Width: 1}, {Width: 1}, {Width: 1}}},
		}, "行序不连续：y 按给定值原样编码"},
		{"cols0_empty_row", 0, []vt.Row{{Y: 0, Cells: nil}}, "cols=0 的空行（头 + y，格流为空）"},
		{"long_grapheme", 5, []vt.Row{{Y: 0, Cells: []vt.Cell{
			vecC("é", nil, nil, 0), // 组合字符（zerowidth 并进 symbol 的形态）
			vecC("e\u0301", nil, nil, 0), {Width: 1}, {Width: 1}, {Width: 1},
		}}}, "多字节 symbol（symLen > 1）"},
	}

	file := vecSurfaceCodecFile{}
	for _, c := range cases {
		rowsIn := make([]vecRowIn, len(c.rows))
		for i, r := range c.rows {
			rowsIn[i].Y = r.Y
			for _, cell := range r.Cells {
				vc := vecCell{Sym: cell.Symbol, Width: cell.Width, Skip: cell.Skip}
				if cell.FG.Kind == vt.ColorPalette {
					vc.FG = []byte{1, cell.FG.Index}
				} else if cell.FG.Kind == vt.ColorRGB {
					vc.FG = []byte{2, cell.FG.R, cell.FG.G, cell.FG.B}
				}
				if cell.BG.Kind == vt.ColorPalette {
					vc.BG = []byte{1, cell.BG.Index}
				} else if cell.BG.Kind == vt.ColorRGB {
					vc.BG = []byte{2, cell.BG.R, cell.BG.G, cell.BG.B}
				}
				if cell.Attr != 0 {
					a := cell.Attr
					vc.Attr = &a
				}
				rowsIn[i].Cells = append(rowsIn[i].Cells, vc)
			}
		}
		file.Cells = append(file.Cells, vecCellCase{
			Name: c.name, Cols: c.cols, RowsIn: rowsIn,
			GridHex: hex.EncodeToString(vt.EncodeGrid(c.cols, uint16(len(c.rows)), c.rows)),
			RowsHex: hex.EncodeToString(vt.EncodeRows(c.rows)),
			Note:    c.note,
		})
	}

	// 负例：篡改合法编码（字节级突变；全部走 DecodeGrid/DecodeRows 报错路径）。
	// blankGood = [01][cols=10 LE][rows=1 LE][y=0 LE][blankRun][varint 10]（9 字节）；
	// cellGood  = [01][cols=1 LE][rows=1 LE][y=0 LE][cell][len 1]['A'][fg 00][bg 00][attr 0000]（14 字节）。
	blankGood := vt.EncodeGrid(10, 1, []vt.Row{{Y: 0, Cells: vecBlank(10)}})
	cellGood := vt.EncodeGrid(1, 1, []vt.Row{{Y: 0, Cells: []vt.Cell{vecC("A", nil, nil, 0)}}})
	mut := func(base []byte, f func(b []byte) []byte) string {
		return hex.EncodeToString(f(append([]byte(nil), base...)))
	}
	file.CellsBad = []vecDecodeBad{
		{Name: "bad_version", InHex: mut(blankGood, func(b []byte) []byte { b[0] = 2; return b }), Note: "版本字节 ≠ 1"},
		{Name: "truncated_header", InHex: hex.EncodeToString(blankGood[:4]), Note: "头都不全"},
		{Name: "bad_marker", InHex: mut(blankGood, func(b []byte) []byte { b[7] = 0x03; return b }), Cols: 10, Count: 1,
			Note: "格流标记 0x03（表外值）"},
		{Name: "bad_color_kind", InHex: mut(cellGood, func(b []byte) []byte { b[10] = 0x03; return b }), Cols: 1, Count: 1,
			Note: "完整格的颜色 kind 0x03"},
		{Name: "repeat_first", InHex: mut(blankGood, func(b []byte) []byte { b[7] = 0x02; return b }), Cols: 10, Count: 1,
			Note: "行首 repeat（无上一格）"},
		{Name: "varint_unterminated", InHex: mut(blankGood, func(b []byte) []byte {
			return append(b[:8], 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80)
		}), Cols: 10, Count: 1, Note: "varint 10 字节未终止"},
		{Name: "row_short", InHex: hex.EncodeToString(blankGood), Cols: 10, Count: 2,
			Note: "DecodeRows 行数声明 > 实有"},
	}

	// ---- 体族（未导出面，包内直调）----

	grid := vt.EncodeGrid(4, 1, []vt.Row{{Y: 0, Cells: vecRep(4, box)}})
	mirror := vt.EncodeGrid(4, 1, []vt.Row{{Y: 0, Cells: vecBlank(4)}})
	rowsEnc := vt.EncodeRows([]vt.Row{{Y: 7, Cells: vecRep(4, box)}})

	file.Snapshot = []vecBodyCase{
		{Name: "minimal", BodyHex: hex.EncodeToString(encSnapshotBody(snapshotBody{
			Geometry: surfaceGeometry{Cols: 80, Rows: 24, Revision: 1},
		})), Note: "全零字段（空 title/grid/mirror 也带长度位）"},
		{Name: "full", BodyHex: hex.EncodeToString(encSnapshotBody(snapshotBody{
			Geometry: surfaceGeometry{Cols: 100, Rows: 32, Revision: 0x01020304},
			Cursor:   surfaceCursor{X: 9, Y: 30, Flags: 0x0f, Shape: 3},
			Modes:    0x000000ff, Kitty: 0x1f, Misc: 1,
			Title:    "标题-title",
			Scroll:   scrollbar{Total: 1234567890123, Offset: 1234567890101, Len: 32},
			Grid:     grid, Mirror: mirror,
		})), Note: "全字段（光标块 6B 含全部 flag 位；title 是原始字节长度）"},
		{Name: "empty_title_nonempty_grid", BodyHex: hex.EncodeToString(encSnapshotBody(snapshotBody{
			Geometry: surfaceGeometry{Cols: 4, Rows: 1, Revision: 7},
			Cursor:   surfaceCursor{X: 3, Y: 0, Flags: 1, Shape: 1},
			Scroll:   scrollbar{Total: 5, Offset: 4, Len: 1},
			Grid:     grid,
		})), Note: "空 title + 有 grid + 无 mirror"},
	}

	file.Diff = []vecBodyCase{
		{Name: "zero_rows", BodyHex: hex.EncodeToString(encDiffBody(diffBody{
			Geometry: surfaceGeometry{Cols: 80, Rows: 24, Revision: 3},
			Cursor:   surfaceCursor{X: 1, Y: 2, Flags: 1, Shape: 0},
			Modes:    0x22, Scroll: scrollbar{Total: 9, Offset: 3, Len: 6},
		})), Note: "rowCount=0 合法帧（只有光标/模式位/回滚条变化）"},
		{Name: "with_rows", BodyHex: hex.EncodeToString(encDiffBody(diffBody{
			Geometry: surfaceGeometry{Cols: 4, Rows: 1, Revision: 8},
			Cursor:   surfaceCursor{X: 2, Y: 0, Flags: 3, Shape: 2},
			Modes:    0xff, Scroll: scrollbar{Total: 4, Offset: 3, Len: 1},
			Rows:     rowsEnc, RowCount: 1,
		})), Note: "带脏行序列"},
	}

	file.FetchReq = []vecBodyCase{
		{Name: "basic", BodyHex: hex.EncodeToString(encFetchRowsReq(fetchRowsReq{From: 7, Count: 12}))},
		{Name: "max", BodyHex: hex.EncodeToString(encFetchRowsReq(fetchRowsReq{From: 0, Count: 512}))},
		{Name: "large_from", BodyHex: hex.EncodeToString(encFetchRowsReq(fetchRowsReq{From: 0xffffffffffffffff, Count: 1}))},
	}
	file.FetchReply = []vecBodyCase{
		{Name: "empty", BodyHex: hex.EncodeToString(encFetchRowsReply(fetchRowsReply{
			Geometry: surfaceGeometry{Cols: 80, Rows: 24, Revision: 2}, From: 1, Count: 0,
		})), Note: "越界截断后空行集"},
		{Name: "with_rows", BodyHex: hex.EncodeToString(encFetchRowsReply(fetchRowsReply{
			Geometry: surfaceGeometry{Cols: 4, Rows: 1, Revision: 9}, From: 0, Count: 1,
			Rows: rowsEnc,
		}))},
	}

	// ---- 分片族（空/单片；多片边界归 Rust 单测——60KiB hex 会膨胀向量）----

	file.Fragment = []vecFragCase{
		{Name: "empty", InHex: "", Frags: fragOf(nil),
			Note: "空数据也发一片（flags=0、data 空）"},
		{Name: "small", InHex: hex.EncodeToString([]byte{1, 2, 3, 0xff}), Frags: fragOf([]byte{1, 2, 3, 0xff})},
	}

	data, err := json.MarshalIndent(file, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, '\n')
	if err := os.WriteFile(filepath.Join(os.Getenv("HOMEWAY_VECGEN_OUT"), "surface_codec.json"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	t.Logf("surface_codec.json：%d 字节", len(data))
}

func fragOf(data []byte) []vecFrag {
	var out []vecFrag
	for _, p := range fragmentPayload(data) {
		flags, chunk, _ := decFragment(p)
		out = append(out, vecFrag{Flags: flags, Hex: hex.EncodeToString(chunk)})
	}
	return out
}
