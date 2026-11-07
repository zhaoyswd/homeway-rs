"use strict";
(() => {
  const form = document.getElementById("login");
  const input = document.getElementById("token");
  const status = document.getElementById("status");
  const pane = document.getElementById("terminal");
  const connect = document.getElementById("connect");
  const disconnect = document.getElementById("disconnect");
  const paste = document.getElementById("paste");
  let socket = null, terminal = null, fit = null, pendingClipboard = null;
  let generation = 0, pendingLogin = null, phase = "idle";
  paste.disabled = true;
  const notice = text => { status.textContent = text; };

  function close() {
    generation++;
    pendingLogin?.abort();
    pendingLogin = null;
    pendingClipboard = null;
    const oldSocket = socket, oldTerminal = terminal;
    socket = null;
    terminal = null;
    oldSocket?.close();
    oldTerminal?.dispose();
    pane.hidden = true;
    form.hidden = false;
    connect.disabled = false;
    paste.disabled = true;
    disconnect.textContent = "断开";
    phase = "idle";
    notice("已断开；工作会话保留，可重新连接");
  }

  form.addEventListener("submit", async event => {
    event.preventDefault();
    const attempt = ++generation;
    phase = "authenticating";
    pendingLogin?.abort();
    const abort = new AbortController();
    pendingLogin = abort;
    connect.disabled = true;
    notice("正在认证目标主机…");
    let token = input.value;
    input.value = "";
    try {
      const response = await fetch("/api/login", {
        method: "POST", headers: {"Content-Type": "application/json"},
        body: JSON.stringify({token}), cache: "no-store", credentials: "omit", signal: abort.signal
      });
      token = "";
      const result = await response.json().catch(() => ({}));
      if (generation !== attempt) return;
      pendingLogin = null;
      if (!response.ok) {
        const messages = {400: "请求或Web凭证格式无效", 401: "Web凭证认证失败或已撤销", 403: "入口拒绝Host或Origin，请检查反代与公开origin", 408: "请求超时，请重试", 413: "请求内容过大", 429: "管理资源忙或请求过多，请稍后重试", 503: "目标管理能力暂不可用，请检查目标与中继", 504: "目标管理连接超时，请稍后重试"};
        throw new Error(result.message || messages[response.status] || "服务暂不可用，请检查入口和目标状态");
      }
      if (typeof result.ticket !== "string") throw new Error("入口未返回有效票据，请升级正在运行的homeway");
      phase = "connecting";
      paste.disabled = true;
      disconnect.textContent = "断开";
      form.hidden = true;
      pane.hidden = false;
      const ui = new Terminal({
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace", fontSize: 14,
        macOptionClickForcesSelection: true,
        theme: {background: "#101419", foreground: "#dce6ed"}, allowProposedApi: true, disableStdin: true
      });
      terminal = ui;
      ui.loadAddon(new Unicode11Addon.Unicode11Addon());
      ui.unicode.activeVersion = "11";
      fit = new FitAddon.FitAddon();
      ui.loadAddon(fit);
      ui.open(pane);
      fit.fit();
      const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`);
      socket = ws;
      ws.binaryType = "arraybuffer";
      const current = () => generation === attempt && socket === ws && terminal === ui;
      let inputInterrupted = false, sentInput = false, everReady = false;
      let lastError = null, exitMessage = null;
      const readOnly = () => { phase = "closed"; ui.options.disableStdin = true; paste.disabled = true; };
      ui.parser.registerOscHandler(52, data => {
        if (!current() || phase !== "ready") return true;
        const split = data.indexOf(";");
        if (split < 0) return true;
        const encoded = data.slice(split + 1);
        if (encoded === "?" || encoded.length > 44000) return true;
        try {
          const value = Uint8Array.from(atob(encoded), c => c.charCodeAt(0));
          if (value.length > 32768) return true;
          pendingClipboard = new TextDecoder().decode(value);
          ui.clearSelection();
          notice("终端请求复制，请点击页面“复制”按钮确认");
        } catch {}
        return true;
      });
      ws.onopen = () => {
        if (!current()) { delete result.ticket; ws.close(); return; }
        ws.send(JSON.stringify({type: "auth", ticket: result.ticket, cols: ui.cols, rows: ui.rows}));
        delete result.ticket;
      };
      ws.onmessage = event => {
        if (!current()) return;
        if (event.data instanceof ArrayBuffer) { if (phase !== "closed") ui.write(new Uint8Array(event.data)); return; }
        let message;
        try { message = JSON.parse(event.data); } catch { return; }
        if (message.type === "ready" && phase === "connecting") {
          everReady = true;
          phase = "ready";
          ui.options.disableStdin = false;
          paste.disabled = false;
          ws.send(JSON.stringify({type: "resize", cols: ui.cols, rows: ui.rows}));
          notice("已连接 · Ctrl+B 快捷指令 · Mac Option/其他平台 Shift 拖选 · 关闭页面保留工作会话");
          ui.focus();
        }
        if (message.type === "error") {
          lastError = message.message || "目标连接中断，请检查目标状态";
          readOnly();
          notice(lastError);
        }
        if (message.type === "exit") {
          exitMessage = `TUI 已退出（${message.status ?? "未知"}）；工作会话保留，可重新连接`;
          readOnly();
          notice(exitMessage);
        }
      };
      ws.onclose = event => {
        delete result.ticket;
        if (!current()) return;
        pendingClipboard = null;
        readOnly();
        connect.disabled = false;
        disconnect.textContent = "重新连接";
        const closeMessages = {1002: "目标管理协议非法，请升级目标", 1008: "认证或授权已失效，请重新获取Web凭证", 1009: "输入超过传输上限，请缩小输入", 1011: "目标终端或连接失效，请检查目标状态", 1013: "目标资源忙，请稍后重试"};
        let message = lastError || exitMessage || closeMessages[event.code] || (everReady ? "连接已断开；工作会话保留，可重新连接" : "连接未建立，请检查反代、公开origin和目标状态");
        if (inputInterrupted || (sentInput && event.code !== 1000 && !exitMessage)) message += "；若刚发送输入，目标可能已收到部分内容，请重连后检查";
        if (!everReady) {
          socket = null;
          terminal = null;
          ui.dispose();
          pane.hidden = true;
          form.hidden = false;
          input.focus();
        }
        if (everReady) message += "；终端已只读，点击“重新连接”可重新登录";
        notice(message);
      };
      ws.onerror = () => {
        if (!current()) return;
        lastError ||= "连接失败，请检查反代、公开origin和目标主机";
        readOnly();
        notice(lastError);
      };
      const inputLimit = 2 * 1024 * 1024;
      const rejectInput = () => notice("输入未发送：本次输入及待发送内容超过 2MiB，请缩小粘贴内容或等待后重试");
      function sendInput(bytes) {
        if (!current() || phase !== "ready" || ws.readyState !== WebSocket.OPEN) return;
        if (bytes.length > inputLimit || ws.bufferedAmount > inputLimit - bytes.length) {
          rejectInput();
          return;
        }
        try {
          for (let offset = 0; offset < bytes.length; offset += 65536) {
            sentInput = true;
            ws.send(bytes.subarray(offset, offset + 65536));
          }
        } catch {
          inputInterrupted = true;
          lastError = "输入传输中断";
          readOnly();
          ws.close();
          notice("输入传输中断，目标可能已收到部分内容；请重连后检查");
        }
      }
      ui.onData(data => {
        if (!current() || phase !== "ready" || ws.readyState !== WebSocket.OPEN) return;
        if (data.length > inputLimit) { rejectInput(); return; }
        sendInput(new TextEncoder().encode(data));
      });
      ui.onBinary(data => {
        if (!current() || phase !== "ready" || ws.readyState !== WebSocket.OPEN) return;
        if (data.length > inputLimit) { rejectInput(); return; }
        sendInput(Uint8Array.from(data, c => c.charCodeAt(0) & 255));
      });
      ui.onResize(({cols, rows}) => { if (current() && phase === "ready" && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({type: "resize", cols, rows})); });
    } catch (error) {
      if (generation !== attempt) return;
      close();
      notice(error.message || "连接失败");
    } finally {
      token = "";
      if (generation === attempt) pendingLogin = null;
    }
  });
  disconnect.addEventListener("click", close);
  document.getElementById("copy").addEventListener("click", async () => {
    const ui = terminal, attempt = generation, pending = pendingClipboard;
    try {
      const text = ui?.getSelection() || pending;
      if (!text) { notice("请先选择终端中的文字"); return; }
      await navigator.clipboard.writeText(text);
      if (terminal !== ui || generation !== attempt) return;
      if (pendingClipboard === pending) pendingClipboard = null;
      notice("已复制");
      ui.focus();
    } catch { if (generation === attempt) notice("浏览器未允许复制，请使用系统复制快捷键"); }
  });
  paste.addEventListener("click", async () => {
    const ui = terminal, attempt = generation;
    if (!ui || phase !== "ready") return;
    try {
      const text = await navigator.clipboard.readText();
      if (terminal !== ui || generation !== attempt || phase !== "ready") return;
      ui.paste(text);
      ui.focus();
    } catch { if (generation === attempt) notice("浏览器未允许读取剪贴板，请使用系统粘贴快捷键"); }
  });
  window.addEventListener("resize", () => { if (terminal) fit.fit(); });
  window.addEventListener("beforeunload", () => socket?.close());
})();
