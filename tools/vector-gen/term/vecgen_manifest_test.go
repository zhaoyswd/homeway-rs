// vecgen_manifest_test.go — homeway-rs R6 6f manifest 检测引擎对照向量生成器。
//
// ⚠️ 本文件是 homeway-rs 仓 tools/vector-gen/term/ 的**模板**，由 tools/gen-vectors.sh 拷进
// baseline 克隆的 pkg/term/manifest/ 再 `go test -run TestVecgenManifest` 触发，
// **绝不 commit 进克隆**。未设 HOMEWAY_VECGEN_OUT 时本用例 skip。
//
// 产出（$HOMEWAY_VECGEN_OUT/term_manifest_eval.json，内容确定性）：
//   regions：手造屏（复用 manifest_test.go 的提示框屏与边界屏）× 全部 region 名 → 切片文本
//            ——逐函数钉 region 层（12 具名 + 3 参数化）。
//   cases： 两份 startup 夹具（testdata/，真 CLI 会话的屏文本）× 22 manifest 的**逐规则**
//            求值轨迹（region/region_bytes/matched/priority/state）+ 终局（状态/可见位/
//            回落标签）——Rust 侧按逐规则对拍（不只对终局状态，评审 A2 口径）。
package manifest

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

type vecRegionCase struct {
	Name   string `json:"name"`
	Screen string `json:"screen"`
	OSCTitle string `json:"osc_title,omitempty"`
	OSCProgress string `json:"osc_progress,omitempty"`
	Region string `json:"region"`
	Want   string `json:"want"`
}

type vecEvalRule struct {
	ID          string `json:"id"`
	Priority    int    `json:"priority"`
	Region      string `json:"region"`
	State       int    `json:"state"`
	Matched     bool   `json:"matched"`
	RegionBytes int    `json:"region_bytes"`
}

type vecEvalCase struct {
	Agent          string         `json:"agent"`
	Fixture        string         `json:"fixture"`
	Rules          []vecEvalRule  `json:"rules"`
	State          string         `json:"state"`
	MatchedRule    string         `json:"matched_rule,omitempty"`
	VisibleIdle    bool           `json:"visible_idle"`
	VisibleBlocker bool           `json:"visible_blocker"`
	VisibleWorking bool           `json:"visible_working"`
	SkipStateUpdate bool          `json:"skip_state_update,omitempty"`
	FallbackReason string         `json:"fallback_reason,omitempty"`
}

// promptBoxScreen 与 manifest_test.go 同款（提示框 + 块标记 + 历史提示的复合屏）。
const vecPromptBoxScreen = "旧的一轮对话输出\n• 已完成上一步\n› 历史输入行\n• 之后又有块标记\n\n───\n│ 提示框正文第一行\n│ 提示框正文第二行\n───\n• 新一轮开始\n› 当前输入\n"

func TestVecgenManifest(t *testing.T) {
	if os.Getenv("HOMEWAY_VECGEN_OUT") == "" {
		t.Skip("HOMEWAY_VECGEN_OUT 未设")
	}

	// ---- ① region 层逐函数 ----

	screens := []struct {
		name          string
		screen        string
		oscTitle      string
		oscProgress   string
		regions       []string
	}{
		{
			name:  "prompt_box",
			screen: vecPromptBoxScreen,
			oscTitle: "codex 工作中", oscProgress: "4;1;42",
			regions: []string{
				"whole_recent", "osc_title", "osc_progress",
				"bottom_lines(2)", "bottom_lines(200)", "bottom_non_empty_lines(1)",
				"bottom_non_empty_lines(2)", "top_non_empty_lines(3)",
				"prompt_box_body", "above_prompt_box", "last_non_empty_above_prompt_box",
				"after_last_horizontal_rule", "current_prompt_block_marker",
				"after_current_prompt_block_marker", "after_last_prompt_marker",
				"before_current_prompt_marker", "whole_recent_without_current_prompt_marker",
				"unknown_region_name",
			},
		},
		{
			name:  "current_prompt_current_input",
			screen: "输出\n› \n当前输入内容",
			regions: []string{"after_last_prompt_marker"},
		},
		{
			name:  "prompt_is_history",
			screen: "输出\n› 历史输入\n• 之后又有块标记\n新输出",
			regions: []string{"whole_recent_without_current_prompt_marker", "before_current_prompt_marker"},
		},
		{
			name:  "current_prompt_tail",
			screen: "输出一\n输出二\n› 当前输入",
			regions: []string{"whole_recent_without_current_prompt_marker", "before_current_prompt_marker"},
		},
		{name: "empty", screen: "", regions: []string{"bottom_lines(3)", "bottom_non_empty_lines(3)", "top_non_empty_lines(2)", "after_last_prompt_marker", "whole_recent"}},
		{name: "all_blank", screen: "\n\n\n", regions: []string{"bottom_non_empty_lines(2)", "top_non_empty_lines(2)", "last_non_empty_above_prompt_box"}},
		{
			name:  "repeat_marker_bottom_wins",
			screen: "marker\nold\n\nmiddle\nmarker\nnew\n",
			regions: []string{"bottom_non_empty_lines(2)", "bottom_non_empty_lines(1)"},
		},
		{
			name:  "no_prompt_no_rule_lines",
			screen: "普通输出一\n普通输出二",
			regions: []string{"current_prompt_block_marker", "after_current_prompt_block_marker", "above_prompt_box", "prompt_box_body"},
		},
		{
			name:  "trailing_newline_only",
			screen: "一行\n",
			regions: []string{"bottom_lines(1)", "top_non_empty_lines(1)", "bottom_non_empty_lines(1)"},
		},
	}

	var regionCases []vecRegionCase
	for _, sc := range screens {
		in := Input{Screen: sc.screen, OSCTitle: sc.oscTitle, OSCProgress: sc.oscProgress}
		for _, r := range sc.regions {
			regionCases = append(regionCases, vecRegionCase{
				Name: sc.name, Screen: sc.screen, OSCTitle: sc.oscTitle, OSCProgress: sc.oscProgress,
				Region: r, Want: region(in, r),
			})
		}
	}

	// ---- ② 两份 startup 夹具 × 全部 manifest 的逐规则轨迹 ----

	l := NewLoader("")
	if ws := l.Warnings(); len(ws) > 0 {
		t.Fatalf("内嵌 manifest 加载告警：%v", ws)
	}
	ids := l.IDs()
	if len(ids) != 22 {
		t.Fatalf("内嵌 manifest 数 %d ≠ 22", len(ids))
	}
	fixtures := []string{"codex-startup.txt", "opencode-startup.txt"}
	var evalCases []vecEvalCase
	for _, fx := range fixtures {
		data, err := os.ReadFile(filepath.Join("testdata", fx))
		if err != nil {
			t.Fatalf("夹具 %s：%v", fx, err)
		}
		screen := string(data)
		for _, id := range ids {
			comp, ok := l.ForID(id)
			if !ok {
				t.Fatalf("ForID(%s) 缺", id)
			}
			res := comp.Evaluate(Input{Screen: screen})
			rules := make([]vecEvalRule, 0, len(res.Rules))
			for _, r := range res.Rules {
				rules = append(rules, vecEvalRule{
					ID: r.ID, Priority: r.Priority, Region: r.Region,
					State: int(r.State), Matched: r.Matched, RegionBytes: r.RegionBytes,
				})
			}
			c := vecEvalCase{
				Agent: id, Fixture: fx, Rules: rules,
				State: res.State.String(),
				VisibleIdle: res.VisibleIdle, VisibleBlocker: res.VisibleBlocker,
				VisibleWorking: res.VisibleWorking, SkipStateUpdate: res.SkipStateUpdate,
				FallbackReason: res.FallbackReason,
			}
			if res.MatchedRule != nil {
				c.MatchedRule = res.MatchedRule.ID
			}
			evalCases = append(evalCases, c)
		}
	}

	out := map[string]any{
		"engine_version": EngineVersion,
		"agents":         ids,
		"regions":        regionCases,
		"cases":          evalCases,
	}
	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	data = append(data, '\n')
	if err := os.WriteFile(filepath.Join(os.Getenv("HOMEWAY_VECGEN_OUT"), "term_manifest_eval.json"), data, 0o644); err != nil {
		t.Fatal(err)
	}
	// 稳定性自检：逐 case 至少一条规则（生成侧不产坏档）。
	for _, c := range evalCases {
		if len(c.Rules) == 0 {
			t.Fatalf("%s/%s：规则轨迹空", c.Agent, c.Fixture)
		}
	}
	t.Logf("term_manifest_eval.json：%d 字节（regions %d + eval %d）", len(data), len(regionCases), len(evalCases))
}
