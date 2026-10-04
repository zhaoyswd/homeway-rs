// vecgen_term_aux.go — homeway-rs R6 向量生成器的伴随文件（由 tools/gen-vectors.sh 拷进
// baseline 克隆的 pkg/term/vt/，跑完删除，**绝不 commit 进克隆**）。
//
// 为什么需要它：Go 不支持在 _test.go 里用 cgo（`use of cgo in test not supported`），
// 生成器测试要用的修饰键/小键盘等键常量不在 keys.go 的导出面里 ⇒ 本文件（普通包文件，
// 可用 cgo）把它们暴露成包级常量供测试引用。残留时只是多几个未用常量，零副作用。
package vt

/*
#cgo CFLAGS: -I${SRCDIR} -I${SRCDIR}/../../../third_party/libghostty-vt/include
#include <ghostty/vt.h>
*/
import "C"

// vecgen 专用键常量（keys.go 导出面之外的输入编码采样键）。
const (
	VecKeyShiftLeft  Key = C.GHOSTTY_KEY_SHIFT_LEFT
	VecKeyCtrlLeft   Key = C.GHOSTTY_KEY_CONTROL_LEFT
	VecKeyAltLeft    Key = C.GHOSTTY_KEY_ALT_LEFT
	VecKeySuperLeft  Key = C.GHOSTTY_KEY_META_LEFT
	VecKeyCapsLock   Key = C.GHOSTTY_KEY_CAPS_LOCK
	VecKeyNumLock    Key = C.GHOSTTY_KEY_NUM_LOCK
	VecKeyNumpad0    Key = C.GHOSTTY_KEY_NUMPAD_0
	VecKeyNumpad1    Key = C.GHOSTTY_KEY_NUMPAD_1
	VecKeyNumpadEnter Key = C.GHOSTTY_KEY_NUMPAD_ENTER
	VecKeyNumpadAdd  Key = C.GHOSTTY_KEY_NUMPAD_ADD
	VecKeyNumpadSub  Key = C.GHOSTTY_KEY_NUMPAD_SUBTRACT
	VecKeyF13        Key = C.GHOSTTY_KEY_F13
)
