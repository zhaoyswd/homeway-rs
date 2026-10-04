//go:build cshared

// vecgen_tunstatus_test.go — tunStatusJSON 的阶段机对照向量（R7-7c）。
// 拷入 baseline 克隆 clientcore/cmd/clientcore/ 运行（跑完由 gen-vectors.sh 删除），
// HOMEWAY_VECGEN_OUT 指向 fixtures/vectors/，产物 tun_status.jsonl。
//
// 覆盖 = 阶段机的全部可达形态（无 runner 期——runner 依赖真会话，键面由 Rust 侧
// 构造输入的键集合守卫钉死，同 Go TestServiceSnapshotJSONReadyWithBridgeKeys 形态）：
// idle / preparing / ready（meowed 与软失败）/ failed（core 与 attach-timeout）/
// demand 三态（未判定/亮屏/位陈旧）/ unhealthyReason。elapsedMs 随墙钟——两侧统一
// 归一为 0 再对账；demand.at 经固定 now 驱动（确定性）。
package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"testing"
	"time"
)

var vtNormalize = regexp.MustCompile(`"elapsedMs":\d+`)

func vtEmit(t *testing.T, name string) {
	out := tunStatusJSON()
	out = vtNormalize.ReplaceAllString(out, `"elapsedMs":0`)
	rec := map[string]string{"name": name, "json": out}
	b, err := json.Marshal(rec)
	if err != nil {
		t.Fatalf("%s: marshal: %v", name, err)
	}
	dir := os.Getenv("HOMEWAY_VECGEN_OUT")
	if dir == "" {
		t.Fatal("HOMEWAY_VECGEN_OUT 未设")
	}
	f, err := os.OpenFile(filepath.Join(dir, "tun_status.jsonl"), os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer f.Close()
	if _, err := f.Write(append(b, '\n')); err != nil {
		t.Fatalf("write: %v", err)
	}
}

func TestVecgenTunStatus(t *testing.T) {
	fixed := time.UnixMilli(1696000000000)

	// ① 初始 idle（进程起点形态；stageSince 归一）
	vtEmit(t, "idle")

	// ② preparing（受理后暖机中）
	setStage(stagePreparing, "", "", false)
	vtEmit(t, "preparing")

	// ③ ready + meowed + readyBy（暖机成功的正形态）
	setStage(stageReady, "", "", true)
	setReadyBy("wg")
	vtEmit(t, "ready_meowed")

	// ④ ready 软失败（暖机窗口内没等到注册确认——仍可 attach 自愈）
	setStage(stageReady, "", "暖机窗口内未收到注册确认", false)
	vtEmit(t, "ready_softfail")

	// ⑤ failed + code=core
	setStage(stageFailed, "core", "新栈启动失败：token 解析失败", false)
	vtEmit(t, "failed_core")

	// ⑥ failed + code=attach-timeout（无人接入而收工）
	setStage(stageFailed, "attach-timeout", "等待接入超时，世代已收工", false)
	vtEmit(t, "failed_attach_timeout")

	// ⑦ demand：未判定（零值——reason 兜「未判定」、at=0）
	setStage(stagePreparing, "", "", false)
	vtEmit(t, "demand_unset")

	// ⑧ demand：亮屏（fresh 位 + 出站包依据）
	setTunActivity(false, true)
	noteDemand(true, "亮屏", fixed)
	vtEmit(t, "demand_screen_on")

	// ⑨ demand：熄屏（位陈旧）+ fg=1 诊断位
	setTunActivity(true, false)
	noteDemand(false, "熄屏（位陈旧）", fixed)
	vtEmit(t, "demand_stale_screen_fg")

	// ⑩ unhealthyReason（patrol 分类）
	tunUnhealthyWhy.Store("patrol")
	vtEmit(t, "unhealthy_patrol")
	tunUnhealthyWhy.Store("")

	// 复位（对后续测试零污染）
	setStage(stageIdle, "", "", false)
	setReadyBy("")
}
