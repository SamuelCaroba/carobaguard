"use strict";

const state = {
  csrf: "",
  user: null,
  samples: [],
  stream: null,
  currentProfile: "balanced",
};

const $ = (id) => document.getElementById(id);

async function request(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (options.body && !headers.has("content-type")) headers.set("content-type", "application/json");
  if (options.method && options.method !== "GET" && state.csrf) headers.set("x-csrf-token", state.csrf);
  const response = await fetch(path, { credentials: "same-origin", ...options, headers });
  const contentType = response.headers.get("content-type") || "";
  const body = contentType.includes("application/json") ? await response.json() : await response.text();
  if (!response.ok) {
    const message = body && body.error ? body.error.message : `HTTP ${response.status}`;
    throw new Error(message);
  }
  return body;
}

function showLogin() {
  $("app").hidden = true;
  $("login-view").hidden = false;
  $("password").focus();
}

function showApp(session) {
  state.user = session.user;
  state.csrf = session.csrf_token;
  $("login-view").hidden = true;
  $("app").hidden = false;
  $("user-name").textContent = session.user.username;
  $("user-role").textContent = session.user.role;
  $("user-initial").textContent = session.user.username.slice(0, 1).toUpperCase();
  startMetrics();
}

async function boot() {
  try {
    showApp(await request("/api/v1/auth/me"));
  } catch (_) {
    showLogin();
  }
}

$("login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  $("login-error").textContent = "";
  const button = event.currentTarget.querySelector("button");
  button.disabled = true;
  try {
    const session = await request("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ username: $("username").value, password: $("password").value }),
    });
    $("password").value = "";
    showApp(session);
  } catch (error) {
    $("login-error").textContent = error.message;
  } finally {
    button.disabled = false;
  }
});

$("logout").addEventListener("click", async () => {
  try { await request("/api/v1/auth/logout", { method: "POST", body: "{}" }); } catch (_) { /* expire locally */ }
  if (state.stream) state.stream.close();
  state.csrf = "";
  showLogin();
});

document.querySelectorAll(".nav-item").forEach((button) => {
  button.addEventListener("click", () => {
    document.querySelectorAll(".nav-item").forEach((item) => item.classList.remove("active"));
    document.querySelectorAll(".page").forEach((page) => page.classList.remove("active"));
    button.classList.add("active");
    const page = button.dataset.page;
    $(`page-${page}`).classList.add("active");
    $("page-title").textContent = button.textContent.trim().replace(/^\d+\s*/, "");
  });
});

$("performance-mode").addEventListener("change", async (event) => {
  const enabled = event.currentTarget.checked;
  try {
    const result = await request("/api/v1/metrics/config", {
      method: "POST",
      body: JSON.stringify({ profile: state.currentProfile, performance_mode: enabled }),
    });
    toast(`Intervalo de telemetria alterado para ${result.interval_seconds}s.`);
  } catch (error) {
    event.currentTarget.checked = !enabled;
    toast(error.message, true);
  }
});

async function startMetrics() {
  try {
    const history = await request("/api/v1/metrics/history?limit=240");
    state.samples = history;
    if (history.length) render(history[history.length - 1]);
    drawHistory();
  } catch (error) {
    toast(error.message, true);
  }
  if (state.stream) state.stream.close();
  state.stream = new EventSource("/api/v1/metrics/events");
  state.stream.addEventListener("metrics", (event) => {
    try {
      const sample = JSON.parse(event.data);
      state.samples.push(sample);
      if (state.samples.length > 240) state.samples.shift();
      render(sample);
      drawHistory();
    } catch (_) { /* malformed server event is ignored */ }
  });
  state.stream.onerror = () => {
    $("system-label").textContent = "RECONECTANDO TELEMETRIA";
  };
}

function render(sample) {
  $("system-label").textContent = `${sample.hostname} · ${sample.os}`;
  $("cpu-percent").textContent = `${sample.cpu_percent.toFixed(1)}%`;
  setBar("cpu-bar", sample.cpu_percent);
  $("cpu-load").textContent = `Load ${sample.load_average.map((n) => n.toFixed(2)).join("  ")}`;
  $("cpu-freq").textContent = sample.cpu_frequency_mhz ? `${sample.cpu_frequency_mhz} MHz` : "Freq. indisponível";
  $("memory-percent").textContent = `${sample.memory.percent.toFixed(1)}%`;
  setBar("memory-bar", sample.memory.percent);
  $("memory-used").textContent = `${bytes(sample.memory.used_bytes)} / ${bytes(sample.memory.total_bytes)}`;
  $("swap-used").textContent = `Swap ${bytes(sample.swap.used_bytes)}`;
  $("disk-percent").textContent = `${sample.disk.percent.toFixed(1)}%`;
  setBar("disk-bar", sample.disk.percent);
  $("disk-used").textContent = `${bytes(sample.disk.used_bytes)} / ${bytes(sample.disk.total_bytes)}`;
  $("disk-io").textContent = `R ${rate(sample.disk.read_bytes_per_sec)} · W ${rate(sample.disk.write_bytes_per_sec)}`;
  $("network-total").textContent = rate(sample.network.received_bytes_per_sec + sample.network.transmitted_bytes_per_sec);
  $("network-rx").textContent = rate(sample.network.received_bytes_per_sec);
  $("network-tx").textContent = rate(sample.network.transmitted_bytes_per_sec);
  $("sample-time").textContent = new Date(sample.sampled_at * 1000).toLocaleTimeString();
  $("hostname").textContent = sample.hostname;
  $("os").textContent = sample.os;
  $("kernel").textContent = sample.kernel;
  $("architecture").textContent = sample.architecture;
  $("uptime").textContent = duration(sample.uptime_seconds);
  $("processes").textContent = String(sample.process_count);
  $("self-cpu").textContent = `${sample.carobaguard.cpu_percent.toFixed(2)}%`;
  $("self-memory").textContent = bytes(sample.carobaguard.memory_bytes);
  $("self-disk").textContent = bytes(sample.carobaguard.disk_bytes);
  $("self-network").textContent = rate(sample.carobaguard.network_bytes_per_sec);
  $("self-writes").textContent = `${sample.carobaguard.db_writes_per_minute.toFixed(1)}/min`;
  $("interval-tag").textContent = `${sample.telemetry_interval_seconds}S`;
  $("performance-mode").checked = sample.performance_mode;
  renderCores(sample.per_core_percent);
  renderTemperatures(sample.temperatures);
}

function renderCores(cores) {
  $("core-count").textContent = `${cores.length} CORES`;
  const rows = cores.map((value, index) => {
    const row = document.createElement("div");
    row.className = "core-row";
    const label = document.createElement("span");
    label.textContent = `CPU${index}`;
    const bar = document.createElement("div");
    bar.className = "bar";
    const fill = document.createElement("i");
    fill.style.width = `${clamp(value)}%`;
    bar.append(fill);
    const amount = document.createElement("b");
    amount.textContent = `${value.toFixed(1)}%`;
    row.append(label, bar, amount);
    return row;
  });
  $("core-list").replaceChildren(...rows);
}

function renderTemperatures(temperatures) {
  if (!temperatures.length) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "Nenhum sensor exposto pelo kernel.";
    $("temperature-list").replaceChildren(empty);
    return;
  }
  const rows = temperatures.map((temperature) => {
    const row = document.createElement("div");
    row.className = "sensor";
    const label = document.createElement("span");
    label.textContent = temperature.label;
    const value = document.createElement("b");
    value.textContent = `${temperature.celsius.toFixed(1)} °C`;
    row.append(label, value);
    return row;
  });
  $("temperature-list").replaceChildren(...rows);
}

function drawHistory() {
  const canvas = $("history-chart");
  const width = Math.max(canvas.clientWidth, 300);
  const height = 190;
  const ratio = Math.min(window.devicePixelRatio || 1, 2);
  canvas.width = width * ratio;
  canvas.height = height * ratio;
  const context = canvas.getContext("2d");
  context.scale(ratio, ratio);
  context.clearRect(0, 0, width, height);
  context.strokeStyle = "#1e2a33";
  context.lineWidth = 1;
  for (let line = 0; line <= 4; line += 1) {
    const y = 8 + ((height - 20) * line) / 4;
    context.beginPath(); context.moveTo(0, y); context.lineTo(width, y); context.stroke();
  }
  if (state.samples.length < 2) return;
  drawSeries(context, state.samples.map((sample) => sample.memory.percent), width, height, "#50a7ff");
  drawSeries(context, state.samples.map((sample) => sample.cpu_percent), width, height, "#45dfa2");
}

function drawSeries(context, values, width, height, color) {
  context.strokeStyle = color;
  context.lineWidth = 1.5;
  context.beginPath();
  values.forEach((value, index) => {
    const x = (index / (values.length - 1)) * width;
    const y = 8 + (1 - clamp(value) / 100) * (height - 20);
    if (index === 0) context.moveTo(x, y); else context.lineTo(x, y);
  });
  context.stroke();
}

function setBar(id, value) { $(id).style.width = `${clamp(value)}%`; }
function clamp(value) { return Math.max(0, Math.min(100, Number(value) || 0)); }

function bytes(value) {
  const amount = Number(value) || 0;
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  if (amount === 0) return "0 B";
  const exponent = Math.min(Math.floor(Math.log(amount) / Math.log(1024)), units.length - 1);
  return `${(amount / 1024 ** exponent).toFixed(exponent > 1 ? 1 : 0)} ${units[exponent]}`;
}

function rate(value) { return `${bytes(value)}/s`; }

function duration(seconds) {
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return `${days}d ${hours}h ${minutes}m`;
}

let toastTimer;
function toast(message, error = false) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").classList.toggle("error", error);
  $("toast").hidden = false;
  toastTimer = setTimeout(() => { $("toast").hidden = true; }, 4000);
}

window.addEventListener("resize", drawHistory);
boot();
