//! `homeway tui` —— 桌面终端工作台（openspec change `add-tui-panel`）。
//!
//! 四区面板：左上主机列表（含本机）/ 左下选中主机的会话列表（agent 状态徽章）/
//! 右侧选中会话的终端视图（可交互）/ 底部状态行（连接形态/延迟/流量）。
//! 设计真源 = `openspec/changes/add-tui-panel/design.md`；边界（A 案）= 控制面词表
//! 与 core 零契约面改动，终端腿不声明 capsRawTerminal（服务端继续代答查询），
//! 输入走客户端 keyenc → DATA 字节帧（不用 INPUT 帧）。

pub(crate) mod app;
pub(crate) mod backend;
pub(crate) mod blit;
pub(crate) mod frame;
pub(crate) mod render;
pub(crate) mod runtime;
pub(crate) mod theme;
pub(crate) mod termview;
pub(crate) mod chan;
pub(crate) mod hosts;
pub(crate) mod input;
pub(crate) mod leg;
pub(crate) mod local;
pub(crate) mod selection;
pub(crate) mod sessions;

/// 命令入口（main.rs 分发）：终端环境边界检查 → daemon 拉起 → 面板主循环。
pub fn cmd_tui(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h" | "help")) {
        tui_usage();
        return;
    }
    std::process::exit(runtime::run(args));
}

fn tui_usage() {
    println!(
        "homeway tui —— 桌面终端工作台（openspec add-tui-panel）\n  四区面板：左上主机列表（含本机）/ 左下会话列表（agent 徽章）/ 右侧终端视图（可交互）/ 底部状态行\n  用法：homeway tui [--state DIR]\n  键位：↑↓ 选择；Tab 换栏；Enter 进入终端焦点（F12 返回）；q 退出\n  终端焦点内按键原样送达会话；多实例同显（选中状态本机共享）"
    );
}

#[cfg(test)]
mod probe {
    use homeway_core::term::keyenc::{Key, KeyAction, KeyEvent, KeyFlavor, Mods};
    use homeway_core::term::vt::SessionVt;

    /// tasks 1.2 编译探针：homeway-cli 侧可直接构造 core 的 `SessionVt` 并取完整键
    /// 编码口径（`key_options()` 含 kitty_flags）——「core 零改动」定稿的依据
    /// （design D2；tasks 2.1 据此保持跳过）。
    #[test]
    fn session_vt_constructible_and_key_encoding_reachable() {
        let vt = SessionVt::new(80, 24, 1000).expect("SessionVt::new（80×24 合法尺寸）");
        let opts = vt.key_options();
        assert_eq!(opts.kitty_flags, 0, "缺省 kitty 键盘协议关闭");
        assert!(opts.alt_esc_prefix, "ghostty 同款缺省：1036 默认 on");
        let ev = KeyEvent {
            key: Key(0x61), // 'a'
            action: KeyAction::Press,
            mods: Mods::NONE,
            text: "a",
            composing: false,
        };
        let bytes = vt.encode_key(&ev, KeyFlavor::host_default());
        assert!(!bytes.is_empty(), "按键编码可达");
    }
}
