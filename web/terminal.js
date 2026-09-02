const MAX_INPUT_BATCH = 64 * 1024;
let terminal = null;
let terminalModule = null;
let socket = null;
let resizeObserver = null;
let inputTimer = null;
let pendingInput = "";
let sessionId = null;
let connectionGeneration = 0;

const element = (id) => document.getElementById(id);

function notify(message, error = false) {
  window.dispatchEvent(new CustomEvent("carobaguard:toast", { detail: { message, error } }));
}

function setStatus(state, label) {
  const status = element("terminal-status");
  if (!status) return;
  status.dataset.state = state;
  status.lastChild.textContent = ` ${label}`;
  element("terminal-connect").disabled = state === "connecting" || state === "connected";
  element("terminal-disconnect").disabled = state !== "connected";
}

async function ensureTerminal() {
  if (terminal) return;
  terminalModule ||= import("/vendor/xterm.mjs");
  const { Terminal } = await terminalModule;
  if (terminal) return;
  terminal = new Terminal({
    allowProposedApi: false,
    convertEol: false,
    cursorBlink: false,
    cursorStyle: "block",
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    fontSize: 12,
    lineHeight: 1.25,
    scrollback: 3000,
    theme: {
      background: "#06090c",
      foreground: "#c4d0d7",
      cursor: "#45dfa2",
      selectionBackground: "#24483d",
      black: "#111820",
      red: "#ff6d78",
      green: "#45dfa2",
      yellow: "#f5ba57",
      blue: "#50a7ff",
      magenta: "#a386ff",
      cyan: "#58d4dd",
      white: "#dce6eb",
    },
  });
  terminal.open(element("terminal-container"));
  terminal.onData(queueInput);
  resizeObserver = new ResizeObserver(() => resizeTerminal());
  resizeObserver.observe(element("terminal-container"));
  element("terminal-container").addEventListener("click", () => terminal.focus());
  resizeTerminal();
}

function dimensions() {
  const container = element("terminal-container");
  return {
    cols: Math.max(2, Math.min(500, Math.floor((container.clientWidth - 28) / 7.25))),
    rows: Math.max(1, Math.min(200, Math.floor((container.clientHeight - 26) / 15))),
  };
}

function resizeTerminal() {
  if (!terminal) return;
  const size = dimensions();
  if (terminal.cols !== size.cols || terminal.rows !== size.rows) terminal.resize(size.cols, size.rows);
  send({ type: "resize", ...size });
}

function queueInput(data) {
  if (!socket || socket.readyState !== WebSocket.OPEN) return;
  if (pendingInput.length + data.length > MAX_INPUT_BATCH) flushInput();
  pendingInput += data;
  if (!inputTimer) inputTimer = window.setTimeout(flushInput, 8);
}

function flushInput() {
  if (inputTimer) window.clearTimeout(inputTimer);
  inputTimer = null;
  if (!pendingInput) return;
  const data = pendingInput;
  pendingInput = "";
  send({ type: "input", data });
}

function send(message) {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
}

async function connect() {
  if (socket && socket.readyState < WebSocket.CLOSING) return;
  const generation = ++connectionGeneration;
  setStatus("connecting", "Conectando");
  try {
    await ensureTerminal();
    if (generation !== connectionGeneration) return;
    terminal.clear();
    terminal.write("\x1b[38;5;244mAbrindo PTY autenticado…\x1b[0m\r\n");
    const response = await fetch("/api/v1/auth/me", { credentials: "same-origin" });
    if (!response.ok) throw new Error("sessão expirada");
    const session = await response.json();
    if (generation !== connectionGeneration) return;
    const size = dimensions();
    const scheme = location.protocol === "https:" ? "wss:" : "ws:";
    const url = `${scheme}//${location.host}/api/v1/terminal/ws?cols=${size.cols}&rows=${size.rows}`;
    const currentSocket = new WebSocket(url, ["carobaguard-v1", `csrf.${session.csrf_token}`]);
    if (generation !== connectionGeneration) {
      currentSocket.close();
      return;
    }
    currentSocket.binaryType = "arraybuffer";
    socket = currentSocket;

    currentSocket.addEventListener("open", () => {
      if (socket !== currentSocket) return;
      setStatus("connected", "Conectado");
      send({ type: "resize", ...dimensions() });
      terminal.focus();
    });
    currentSocket.addEventListener("message", (event) => handleMessage(currentSocket, event));
    currentSocket.addEventListener("error", () => {
      if (socket !== currentSocket) return;
      setStatus("error", "Falha");
    });
    currentSocket.addEventListener("close", () => {
      if (socket !== currentSocket) return;
      socket = null;
      sessionId = null;
      pendingInput = "";
      setStatus("disconnected", "Desconectado");
      element("terminal-session").textContent = "Nenhuma sessão ativa";
    });
  } catch (error) {
    if (generation !== connectionGeneration) return;
    socket = null;
    setStatus("error", "Falha");
    terminal.write(`\r\n\x1b[31m${error.message}\x1b[0m\r\n`);
    notify(error.message, true);
  }
}

function handleMessage(currentSocket, event) {
  if (socket !== currentSocket) return;
  if (event.data instanceof ArrayBuffer) {
    terminal.write(new Uint8Array(event.data));
    return;
  }
  let message;
  try { message = JSON.parse(event.data); }
  catch (_) { return; }
  if (message.type === "status") {
    sessionId = message.session_id;
    const pid = message.pid ? ` · PID ${message.pid}` : "";
    element("terminal-session").textContent = `Sessão ${sessionId.slice(0, 8)}${pid}`;
  } else if (message.type === "error") {
    terminal.write(`\r\n\x1b[31m${message.message}\x1b[0m\r\n`);
    notify(message.message, true);
  } else if (message.type === "exit") {
    terminal.write(`\r\n\x1b[38;5;244mSessão encerrada: ${message.reason}\x1b[0m\r\n`);
  }
}

function disconnect() {
  connectionGeneration += 1;
  flushInput();
  if (socket && socket.readyState < WebSocket.CLOSING) socket.close(1000, "user disconnect");
  socket = null;
  sessionId = null;
  setStatus("disconnected", "Desconectado");
  if (element("terminal-session")) element("terminal-session").textContent = "Nenhuma sessão ativa";
}

function activate() {
  ensureTerminal()
    .then(() => window.setTimeout(() => resizeTerminal(), 0))
    .catch((error) => notify(error.message, true));
}

function reset() {
  disconnect();
  terminal?.reset();
  terminal?.clear();
}

element("terminal-connect").addEventListener("click", connect);
element("terminal-disconnect").addEventListener("click", disconnect);
window.addEventListener("beforeunload", disconnect);
window.carobaguardTerminal = { activate, disconnect, reset };
