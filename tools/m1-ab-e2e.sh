#!/bin/zsh
# **已退役（M5 C4）**：本脚本 = M1 S5-2/S5-3 的 wg|quic 产品路径 A/B——其**唯一变量 = 承载开关**
# （`HOMEWAY_TRANSPORT=wg|quic` + `m1-ab --transport wg` 的 WG 臂）。M5 C4 按主会话裁决删掉了
# 承载三键（设计 §4 / §15-5「三个全删」）、S1b 退役了 boringtun 与 WG 实验臂 ⇒ **WG 臂不存在、
# 开关不存在**，本脚本的判据面（A/B 差值）无对象 ⇒ 退役。
# 历史读数仍在册：`docs/reviews/M1.md`（§4.2/§5）/ `docs/reviews/M2.md`（§5/§9）与
# `docs/PERF-AB.md`；**跨期不可互引**（档位/承载口径已变——M5 登记）。
# QUIC 单承载的对应读数面 = `tools/quic-ab.sh`（lab 臂 + size）+ `tools/local-rust-exit.sh` 的
# 冒烟 + 各 `tools/quic-*-e2e.sh`。
set -u
print -r -- "tools/m1-ab-e2e.sh 已退役（M5 C4：WG 承载臂与承载开关删除）——见文件头说明；" >&2
print -r -- "历史读数见 docs/reviews/M1.md、docs/reviews/M2.md、docs/PERF-AB.md（跨期不可互引）。" >&2
exit 2
