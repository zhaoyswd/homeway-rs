#!/usr/bin/env python3
"""Offline current-frame regression for tui-remote-smoke host status matching."""

from pathlib import Path
import runpy
import unittest

import pyte


smoke = runpy.run_path(str(Path(__file__).with_name("tui-remote-smoke.py")))
host_ready = smoke["host_ready"]
Tui = smoke["Tui"]
sidebar_width = smoke["helpers"]["sidebar_width"]
wcwidth = smoke["helpers"]["wcwidth"]


def frame(name="home", detail="  direct 0ms proxy:off", *, columns=100, selected=False):
    tui = object.__new__(Tui)
    tui.screen = pyte.Screen(columns, 12)
    tui._stub_diagnostics = set()
    tui.output = bytearray(b"old frame: home direct relay offline")
    stream = pyte.Stream(tui.screen)
    sidebar = sidebar_width(columns)

    def write(row, text, bold=False):
        cropped, width = "", 0
        for char in text:
            width += max(0, wcwidth(char))
            if width > sidebar - 3:
                break
            cropped += char
        stream.feed(f"\x1b[{row + 1};1H│ \x1b[{'1' if bold else '0'}m{cropped}\x1b[0m")
        stream.feed(f"\x1b[{row + 1};{sidebar}H│")

    stream.feed("\x1b[1;1H┌ 主机 ")
    write(1, f"● {name}", selected)
    write(2, detail)
    stream.feed("\x1b[6;1H┌ 会话 ")
    tui._test_stream = stream
    tui._test_write = write
    return tui


class HostStatusTests(unittest.TestCase):
    def test_remote_normal_states_and_online_gate(self):
        for state in ("direct", "relay", "offline"):
            for rtt in ("0ms", "12ms", "--ms"):
                with self.subTest(state=state, rtt=rtt):
                    tui = frame(detail=f"  {state} {rtt} proxy:off", selected=True)
                    self.assertTrue(host_ready(tui, "home", selected=True))
                    self.assertEqual(host_ready(tui, "home", online=True), state != "offline")

    def test_physical_narrow_and_proxy_or_rtt_crop(self):
        self.assertTrue(host_ready(frame(name="tui-remote"), "tui-remote"))
        for columns in (40, 70, 100, 140):
            for detail in ("  direct 0ms proxy:off", "  relay 123456ms proxy:on", "  offline --ms proxy:off"):
                with self.subTest(columns=columns, detail=detail):
                    self.assertTrue(host_ready(frame(detail=detail, columns=columns), "home"))

    def test_exact_name_selection_and_local_status(self):
        self.assertFalse(host_ready(frame(), "hom"))
        self.assertFalse(host_ready(frame(), "home-extra"))
        self.assertFalse(host_ready(frame(), "home", selected=True))
        self.assertTrue(host_ready(frame(selected=True), "home", selected=True))
        for columns in (40, 100, 140):
            for exit_state in ("on", "off"):
                for relay_state in ("on", "off"):
                    tui = frame(name="本机", columns=columns, selected=True,
                                detail=f"  exit:{exit_state} relay:{relay_state}")
                    self.assertTrue(host_ready(tui, "本机", selected=True, local=True))
                    self.assertFalse(host_ready(tui, "本机", online=True))
        self.assertFalse(host_ready(frame(), "home", local=True))

    def test_invalid_or_prematurely_truncated_status(self):
        for detail in ("  direct nonsense", "  direct 0ms proxy:of", "  direct 0ms", "direct 0ms proxy:off",
                       "  offlineish 0ms proxy:off", "  direct 0ms proxy:off trailing", "  exit:bad relay:off"):
            with self.subTest(detail=detail):
                self.assertFalse(host_ready(frame(detail=detail, columns=140), "home"))
        tui = frame()
        tui._test_write(2, "● other")
        self.assertFalse(host_ready(tui, "home"))
        self.assertFalse(smoke["host_status_visible"]("  direct -m", 11))
        self.assertFalse(smoke["host_status_visible"]("  dir", 5, online=True))
        self.assertFalse(host_ready(frame(name="【直连】home"), "home"))

    def test_only_current_host_panel_and_adjacent_row(self):
        tui = frame(name="other")
        sidebar = sidebar_width(tui.screen.columns)
        tui._test_stream.feed(f"\x1b[2;{sidebar + 4}H● home\x1b[3;{sidebar + 4}H  direct 0ms proxy:off")
        self.assertFalse(host_ready(tui, "home"))
        tui._test_write(6, "● home", True)
        tui._test_write(7, "  direct 0ms proxy:off")
        self.assertFalse(host_ready(tui, "home"))
        tui._test_stream.feed("\x1b[1;1Hnot hosts")
        self.assertFalse(host_ready(tui, "other"))
        tui = frame()
        tui._test_stream.feed(f"\x1b[2;{sidebar}HX")
        self.assertFalse(host_ready(tui, "home"))

    def test_overlay_and_stale_output_do_not_count(self):
        for overlay in ("┌ 添加主机 ", "┌ 快捷指令 · Ctrl+B "):
            tui = frame()
            tui._test_stream.feed(f"\x1b[4;30H{overlay}")
            self.assertFalse(host_ready(tui, "home"))
        tui = frame(name="other")
        self.assertFalse(host_ready(tui, "home"))


if __name__ == "__main__":
    unittest.main()
