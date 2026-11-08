"use strict";
(() => {
  const form = document.getElementById("login");
  const input = document.getElementById("token");
  const status = document.getElementById("status");
  const pane = document.getElementById("terminal");
  const connect = document.getElementById("connect");
  const disconnect = document.getElementById("disconnect");
  const reconnect = document.getElementById("reconnect");
  const paste = document.getElementById("paste");
  let socket = null, terminal = null, fit = null, pendingClipboard = null;
  let generation = 0, pendingLogin = null, phase = "idle";
  // Browser recovery policy; server Ping/Pong already keeps idle WS connections alive.
  const attemptTimeout = 20000, stableConnection = 30000, authRecoveryWindow = 90000;
  let credential = "", retryTimer = null, deadlineTimer = null, retryCount = 0;
  let authRecoveryUntil = 0, authFailures = 0, inputWarning = "";
  paste.disabled = true;
  const notice = text => { status.textContent = text; };

  function rememberToken(token) {
    const url = new URL(location.href);
    const fragment = new URLSearchParams(url.hash.slice(1));
    if (token) fragment.set("webtoken", token);
    else fragment.delete("webtoken");
    url.hash = fragment.toString();
    history.replaceState(null, "", url);
  }

  function dropConnection(keepTerminal = false) {
    clearTimeout(deadlineTimer);
    deadlineTimer = null;
    pendingLogin?.abort();
    pendingLogin = null;
    pendingClipboard = null;
    const oldSocket = socket;
    socket = null;
    oldSocket?.close();
    paste.disabled = true;
    if (terminal) terminal.options.disableStdin = true;
    if (!keepTerminal) {
      terminal?.dispose();
      terminal = null;
      fit = null;
      pane.replaceChildren();
    }
  }

  function close() {
    generation++;
    clearTimeout(retryTimer);
    retryTimer = null;
    dropConnection();
    credential = "";
    input.value = "";
    rememberToken("");
    retryCount = 0;
    authRecoveryUntil = 0;
    authFailures = 0;
    inputWarning = "";
    pane.hidden = true;
    form.hidden = false;
    connect.disabled = false;
    disconnect.textContent = "断开";
    reconnect.hidden = true;
    phase = "idle";
    notice("已断开；工作会话保留，可重新连接");
  }

  function failed(message, retry, attempt) {
    if (generation !== attempt) return;
    clearTimeout(deadlineTimer);
    deadlineTimer = null;
    clearTimeout(retryTimer);
    retryTimer = null;
    phase = "closed";
    paste.disabled = true;
    pendingClipboard = null;
    if (terminal) terminal.options.disableStdin = true;
    connect.disabled = false;
    if (!terminal) { pane.hidden = true; form.hidden = false; }
    if (!retry || !credential) {
      reconnect.hidden = !credential;
      disconnect.textContent = "断开";
      notice(message + inputWarning);
      return;
    }
    const delay = Math.min(30000, 1000 * 2 ** Math.min(retryCount++, 5)) + Math.floor(Math.random() * 251);
    phase = "retrying";
    disconnect.textContent = "取消重连";
    notice(`${message}；${Math.ceil(delay / 1000)}秒后自动重连` + inputWarning);
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (generation === attempt) startConnection();
    }, delay);
  }

  async function startConnection() {
    const attempt = ++generation;
    clearTimeout(retryTimer);
    retryTimer = null;
    dropConnection(true);
    if (authFailures > 0 && Date.now() >= authRecoveryUntil) {
      failed("Web凭证认证未恢复，请重新获取凭证或检查目标状态", false, attempt);
      return;
    }
    phase = "authenticating";
    const abort = new AbortController();
    pendingLogin = abort;
    connect.disabled = true;
    disconnect.textContent = "断开";
    reconnect.hidden = true;
    notice("正在认证目标主机…" + inputWarning);
    let token = credential;
    let retryable = true;
    deadlineTimer = setTimeout(() => abort.abort(), attemptTimeout);
    try {
      const response = await fetch("/api/login", {
        method: "POST", headers: {"Content-Type": "application/json"},
        body: JSON.stringify({token}), cache: "no-store", credentials: "omit", redirect: "manual", signal: abort.signal
      });
      token = "";
      const result = await response.json().catch(error => {
        if (error.name === "AbortError") throw error;
        return null;
      });
      if (generation !== attempt) return;
      clearTimeout(deadlineTimer);
      deadlineTimer = null;
      pendingLogin = null;
      if (!response.ok) {
        retryable = [408, 429].includes(response.status) || response.status >= 500;
        if (response.status === 401) {
          authFailures++;
          retryable = authFailures < 6 && Date.now() < authRecoveryUntil;
        }
        const messages = {400: "请求或Web凭证格式无效", 401: "Web凭证认证失败或已撤销", 403: "入口拒绝Host或Origin，请检查反代与公开origin", 408: "请求超时，请重试", 413: "请求内容过大", 429: "管理资源忙或请求过多，请稍后重试", 503: "目标管理能力暂不可用，请检查目标与中继", 504: "目标管理连接超时，请稍后重试"};
        throw new Error(result?.message || messages[response.status] || "服务暂不可用，请检查入口和目标状态");
      }
      if (!result || typeof result !== "object" || Array.isArray(result) || typeof result.ticket !== "string" || !result.ticket.trim()) {
        retryable = false;
        throw new Error("入口未返回有效票据，请升级正在运行的homeway");
      }
      phase = "connecting";
      paste.disabled = true;
      disconnect.textContent = "断开";
      form.hidden = true;
      pane.hidden = false;
      terminal?.dispose();
      pane.replaceChildren();
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
      let lastError = null, exitMessage = null, fatalError = false, readyAt = 0;
      deadlineTimer = setTimeout(() => {
        if (!current() || phase === "ready") return;
        lastError = "连接建立超时";
        readOnly();
        dropConnection(true);
        failed(lastError, true, attempt);
      }, attemptTimeout);
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
          clearTimeout(deadlineTimer);
          deadlineTimer = null;
          everReady = true;
          readyAt = Date.now();
          authRecoveryUntil = 0;
          authFailures = 0;
          rememberToken(credential);
          phase = "ready";
          ui.options.disableStdin = false;
          paste.disabled = false;
          ws.send(JSON.stringify({type: "resize", cols: ui.cols, rows: ui.rows}));
          notice("已连接 · Ctrl+B 快捷指令 · Mac Option/其他平台 Shift 拖选 · 关闭页面保留工作会话" + inputWarning);
          inputWarning = "";
          ui.focus();
        }
        if (message.type === "error") {
          lastError = message.message || "目标连接中断，请检查目标状态";
          fatalError = ["revoked", "terminal_unavailable", "protocol_error"].includes(message.code);
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
        clearTimeout(deadlineTimer);
        deadlineTimer = null;
        pendingClipboard = null;
        readOnly();
        const closeMessages = {1002: "目标管理协议非法，请升级目标", 1008: "认证或授权已失效，请重新获取Web凭证", 1009: "输入超过传输上限，请缩小输入", 1011: "目标终端或连接失效，请检查目标状态", 1013: "目标资源忙，请稍后重试"};
        let message = lastError || exitMessage || closeMessages[event.code] || (everReady ? "连接已断开；工作会话保留" : "连接未建立，请检查反代、公开origin和目标状态");
        if (inputInterrupted || (sentInput && event.code !== 1000 && !exitMessage)) inputWarning = "；若刚发送输入，目标可能已收到部分内容，请重连后检查";
        if (everReady) {
          authRecoveryUntil = Date.now() + authRecoveryWindow;
          authFailures = 0;
          if (Date.now() - readyAt >= stableConnection) retryCount = 0;
        } else {
          terminal = null;
          ui.dispose();
          pane.replaceChildren();
        }
        if (event.code === 1008 && !everReady) authFailures++;
        const recoveringAuth = authFailures < 6 && Date.now() < authRecoveryUntil;
        const retry = !fatalError && !exitMessage && ![1000, 1002, 1009].includes(event.code) && (event.code !== 1008 || recoveringAuth);
        if (event.code === 1008 && retry && !lastError) message = "连接暂时中断，正在重新认证";
        failed(message, retry, attempt);
      };
      ws.onerror = () => {
        if (!current()) return;
        lastError ||= "连接失败，请检查反代、公开origin和目标主机";
        readOnly();
        ws.close(4000, "connection failed");
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
          ws.close(4000, "input interrupted");
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
      dropConnection(true);
      failed(error.name === "AbortError" ? "连接请求超时" : error.message || "连接失败", retryable, attempt);
    } finally {
      token = "";
      if (generation === attempt) pendingLogin = null;
    }
  }
  form.addEventListener("submit", event => {
    event.preventDefault();
    credential = input.value.trim();
    input.value = "";
    authRecoveryUntil = 0;
    authFailures = 0;
    retryCount = 0;
    inputWarning = "";
    startConnection();
  });
  // Cancel only the browser default; xterm still delivers right-button mouse input to TUI.
  pane.addEventListener("contextmenu", event => event.preventDefault(), {passive: false});
  reconnect.addEventListener("click", () => {
    authRecoveryUntil = 0;
    authFailures = 0;
    retryCount = 0;
    if (credential) startConnection();
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
  window.addEventListener("pagehide", () => {
    generation++;
    clearTimeout(retryTimer);
    retryTimer = null;
    dropConnection();
    phase = "idle";
  });
  window.addEventListener("pageshow", event => { if (event.persisted && credential) startConnection(); });
  window.addEventListener("online", () => { if (phase === "retrying" && credential) startConnection(); });
  credential = new URLSearchParams(location.hash.slice(1)).get("webtoken")?.trim() || "";
  if (credential) startConnection();
})();
