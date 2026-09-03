"use strict";

const state = {
  csrf: "",
  user: null,
  samples: [],
  stream: null,
  currentProfile: "balanced",
  units: [],
  aiSession: null,
  aiStream: null,
  aiMode: "read_only",
  aiContext: { kind: "system", target: null, label: "System overview" },
  pendingPermissions: [],
  unrestrictedConfirmed: false,
  logStream: null,
  logLines: [],
  logPaused: false,
  logTargets: { systemd: [], docker: [] },
  logRenderTimer: null,
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
  window.carobaguardTerminal?.reset();
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
  window.carobaguardTerminal?.reset();
  try { await request("/api/v1/auth/logout", { method: "POST", body: "{}" }); } catch (_) { /* expire locally */ }
  if (state.stream) state.stream.close();
  if (state.aiStream) state.aiStream.close();
  stopLogStream(false);
  state.stream = null;
  state.aiStream = null;
  state.aiSession = null;
  clearPendingPermissions();
  state.unrestrictedConfirmed = false;
  state.csrf = "";
  state.user = null;
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
    loadPage(page);
  });
});

function loadPage(page) {
  if (page === "terminal") window.carobaguardTerminal?.activate();
  else window.carobaguardTerminal?.disconnect();
  if (page === "logs") loadLogs();
  else stopLogStream(false);
  if (page === "docker") loadDocker();
  if (page === "services") loadServices();
  if (page === "audit") loadAudit();
  if (page === "doctor") loadDoctor();
  if (page === "ai") loadAi();
  if (page === "projects") loadProjects();
}

window.addEventListener("carobaguard:toast", (event) => {
  toast(event.detail?.message || "Erro no terminal.", Boolean(event.detail?.error));
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

async function loadDocker() {
  $("docker-status").textContent = "Verificando socket…";
  setTableMessage("docker-rows", 6, "Carregando containers…");
  try {
    const status = await request("/api/v1/docker/status");
    if (!status.available) {
      $("docker-status").textContent = `Indisponível · ${status.error || status.socket}`;
      setTableMessage("docker-rows", 6, "Docker Engine não está acessível para este daemon.");
      return;
    }
    $("docker-status").textContent = `Docker ${status.version} · ${status.socket}`;
    const containers = await request("/api/v1/docker/containers");
    renderContainers(containers);
  } catch (error) {
    $("docker-status").textContent = error.message;
    setTableMessage("docker-rows", 6, "Falha ao consultar Docker.");
  }
}

function renderContainers(containers) {
  if (!containers.length) {
    setTableMessage("docker-rows", 6, "Nenhum container encontrado.");
    return;
  }
  const rows = containers.map((container) => {
    const row = document.createElement("tr");
    row.append(
      tableCell(container.name, "primary-cell"),
      tableCell(container.image, "secondary-cell"),
      statusCell(container.state, container.status),
      tableCell(container.compose_project || "—"),
      tableCell(new Date(container.created * 1000).toLocaleDateString()),
      actionCell(container),
    );
    return row;
  });
  $("docker-rows").replaceChildren(...rows);
}

function actionCell(container) {
  const cell = document.createElement("td");
  cell.className = "align-right";
  const actions = document.createElement("div");
  actions.className = "row-actions";
  actions.append(actionButton("Logs", () => openDockerLogs(container.id, container.name)));
  if (container.state === "running") {
    actions.append(actionButton("Restart", () => mutateContainer(container.id, "restart")));
    actions.append(actionButton("Stop", () => mutateContainer(container.id, "stop"), "danger"));
  } else {
    actions.append(actionButton("Start", () => mutateContainer(container.id, "start")));
  }
  actions.append(actionButton("Ask AI", () => openAiFor("container", container.id, container.name), "ai"));
  cell.append(actions);
  return cell;
}

async function mutateContainer(id, action) {
  try {
    await request(`/api/v1/docker/containers/${encodeURIComponent(id)}/actions`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    toast(`Container: ${action} solicitado.`);
    await loadDocker();
  } catch (error) { toast(error.message, true); }
}

async function openDockerLogs(id, name) {
  openLogDialog(`Container · ${name}`, "Docker Engine");
  try {
    $("log-output").textContent = await request(`/api/v1/docker/containers/${encodeURIComponent(id)}/logs?tail=500`);
  } catch (error) { $("log-output").textContent = `Erro: ${error.message}`; }
}

async function loadServices() {
  $("services-status").textContent = "Verificando systemd…";
  setTableMessage("service-rows", 5, "Carregando serviços…");
  try {
    const status = await request("/api/v1/services/status");
    if (!status.available) {
      $("services-status").textContent = `Indisponível · ${status.error || "systemctl ausente"}`;
      setTableMessage("service-rows", 5, "systemd não está disponível neste host.");
      return;
    }
    $("services-status").textContent = `System state: ${status.state}${status.error ? ` · ${status.error}` : ""}`;
    state.units = await request("/api/v1/services");
    renderServices();
  } catch (error) {
    $("services-status").textContent = error.message;
    setTableMessage("service-rows", 5, "Falha ao consultar systemd.");
  }
}

function renderServices() {
  const filter = $("service-filter").value.trim().toLocaleLowerCase();
  const units = state.units.filter((unit) => !filter || `${unit.name} ${unit.description}`.toLocaleLowerCase().includes(filter));
  if (!units.length) {
    setTableMessage("service-rows", 5, "Nenhum serviço corresponde ao filtro.");
    return;
  }
  const rows = units.map((unit) => {
    const row = document.createElement("tr");
    const actions = document.createElement("td");
    actions.className = "align-right";
    const buttons = document.createElement("div");
    buttons.className = "row-actions";
    buttons.append(actionButton("Logs", () => openServiceLogs(unit.name)));
    if (unit.active_state === "active") {
      buttons.append(actionButton("Restart", () => mutateService(unit.name, "restart")));
      buttons.append(actionButton("Stop", () => mutateService(unit.name, "stop"), "danger"));
    } else {
      buttons.append(actionButton("Start", () => mutateService(unit.name, "start")));
    }
    buttons.append(actionButton("Ask AI", () => openAiFor("service", unit.name, unit.name), "ai"));
    actions.append(buttons);
    row.append(
      tableCell(unit.name, "primary-cell"),
      tableCell(unit.description, "secondary-cell"),
      statusCell(unit.active_state, unit.active_state),
      tableCell(unit.sub_state),
      actions,
    );
    return row;
  });
  $("service-rows").replaceChildren(...rows);
}

async function mutateService(unit, action) {
  try {
    await request(`/api/v1/services/${encodeURIComponent(unit)}/actions`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    toast(`Serviço: ${action} solicitado.`);
    await loadServices();
  } catch (error) { toast(error.message, true); }
}

async function openServiceLogs(unit) {
  openLogDialog(`Serviço · ${unit}`, "systemd journal");
  try {
    $("log-output").textContent = await request(`/api/v1/services/${encodeURIComponent(unit)}/logs?lines=500`);
  } catch (error) { $("log-output").textContent = `Erro: ${error.message}`; }
}

async function loadLogs() {
  stopLogStream(false);
  $("stream-log-status").textContent = "Atualizando fontes de log…";
  const [dockerResult, systemdResult] = await Promise.allSettled([
    request("/api/v1/docker/containers"),
    request("/api/v1/services"),
  ]);
  state.logTargets.docker = dockerResult.status === "fulfilled"
    ? dockerResult.value.map((container) => ({ value: container.id, label: container.name }))
    : [];
  state.logTargets.systemd = systemdResult.status === "fulfilled"
    ? systemdResult.value.map((unit) => ({ value: unit.name, label: unit.name }))
    : [];
  renderLogTargets();
  const count = state.logTargets.docker.length + state.logTargets.systemd.length;
  $("stream-log-status").textContent = count
    ? `${count} fontes disponíveis · streaming parado`
    : "Docker e systemd não forneceram fontes de log.";
}

function renderLogTargets() {
  const source = $("stream-log-source").value;
  const previousTarget = $("stream-log-target").value;
  const targets = state.logTargets[source] || [];
  const options = targets.map((target) => {
    const option = document.createElement("option");
    option.value = target.value;
    option.textContent = target.label;
    return option;
  });
  if (!options.length) {
    const option = document.createElement("option");
    option.value = "";
    option.textContent = "Nenhuma fonte disponível";
    options.push(option);
  }
  $("stream-log-target").replaceChildren(...options);
  if (targets.some((target) => target.value === previousTarget)) {
    $("stream-log-target").value = previousTarget;
  }
}

function startLogStream() {
  const source = $("stream-log-source").value;
  const target = $("stream-log-target").value;
  if (!target) {
    toast("Selecione uma fonte de log disponível.", true);
    return;
  }
  stopLogStream(false);
  state.logLines = [];
  state.logPaused = false;
  renderStreamLogs();
  const query = new URLSearchParams({ source, target, tail: "300" });
  const stream = new EventSource(`/api/v1/logs/events?${query}`);
  let terminalMessage = false;
  state.logStream = stream;
  $("pause-log-stream").disabled = false;
  $("pause-log-stream").textContent = "Pausar";
  $("stream-log-status").textContent = `Conectando · ${source} · ${target}`;
  stream.onopen = () => {
    if (state.logStream === stream) $("stream-log-status").textContent = `LIVE · ${source} · ${target}`;
  };
  stream.addEventListener("line", (event) => {
    if (state.logStream !== stream) return;
    try {
      const message = JSON.parse(event.data);
      appendStreamLog(message.line || "");
    } catch (_) { /* ignore malformed event */ }
  });
  stream.addEventListener("status", (event) => {
    if (state.logStream !== stream) return;
    terminalMessage = true;
    try { $("stream-log-status").textContent = JSON.parse(event.data).message || "Stream encerrado"; }
    catch (_) { $("stream-log-status").textContent = "Stream encerrado"; }
  });
  stream.addEventListener("stream_error", (event) => {
    if (state.logStream !== stream) return;
    terminalMessage = true;
    try { $("stream-log-status").textContent = JSON.parse(event.data).message || "Falha no stream"; }
    catch (_) { $("stream-log-status").textContent = "Falha no stream"; }
  });
  stream.onerror = () => {
    if (state.logStream !== stream) return;
    stream.close();
    state.logStream = null;
    $("pause-log-stream").disabled = true;
    if (!terminalMessage) {
      $("stream-log-status").textContent = "Stream desconectado";
    }
  };
}

function stopLogStream(showStatus = true) {
  if (state.logStream) state.logStream.close();
  state.logStream = null;
  state.logPaused = false;
  const pause = $("pause-log-stream");
  if (pause) {
    pause.disabled = true;
    pause.textContent = "Pausar";
  }
  if (showStatus && $("stream-log-status")) $("stream-log-status").textContent = "Stream parado";
}

function appendStreamLog(line) {
  state.logLines.push(String(line));
  if (state.logLines.length > 5000) state.logLines.splice(0, state.logLines.length - 5000);
  $("stream-log-count").textContent = `${state.logLines.length.toLocaleString()} linhas`;
  if (state.logPaused || state.logRenderTimer) return;
  state.logRenderTimer = window.setTimeout(() => {
    state.logRenderTimer = null;
    renderStreamLogs();
  }, 100);
}

function visibleStreamLogs() {
  const filter = $("stream-log-filter").value.trim().toLocaleLowerCase();
  const level = $("stream-log-level").value;
  return state.logLines.filter((line) => {
    if (filter && !line.toLocaleLowerCase().includes(filter)) return false;
    return level === "all" || classifyLogLevel(line) === level;
  });
}

function classifyLogLevel(line) {
  const value = line.toLocaleLowerCase();
  if (/\b(error|fatal|failed|failure|panic)\b/.test(value)) return "error";
  if (/\b(warn|warning)\b/.test(value)) return "warning";
  return "info";
}

function renderStreamLogs() {
  const output = $("stream-log-output");
  const follow = output.scrollHeight - output.scrollTop - output.clientHeight < 40;
  const lines = visibleStreamLogs();
  output.textContent = lines.length ? lines.join("\n") : "Nenhuma linha corresponde aos filtros.";
  if (follow) output.scrollTop = output.scrollHeight;
}

function toggleLogPause() {
  if (!state.logStream) return;
  state.logPaused = !state.logPaused;
  $("pause-log-stream").textContent = state.logPaused ? "Continuar" : "Pausar";
  $("stream-log-status").textContent = state.logPaused ? "PAUSED · recebimento continua com buffer limitado" : "LIVE";
  if (!state.logPaused) renderStreamLogs();
}

function clearStreamLogs() {
  state.logLines = [];
  $("stream-log-count").textContent = "0 linhas";
  renderStreamLogs();
}

function exportStreamLogs() {
  const content = visibleStreamLogs().join("\n");
  const blob = new Blob([content], { type: "text/plain;charset=utf-8" });
  const link = document.createElement("a");
  link.href = URL.createObjectURL(blob);
  link.download = `carobaguard-logs-${new Date().toISOString().replaceAll(":", "-")}.log`;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(link.href), 0);
}

async function loadProjects() {
  setTableMessage("project-rows", 6, "Inspecionando projetos…");
  try {
    const projects = await request("/api/v1/projects");
    $("projects-status").textContent = projects.length
      ? `${projects.length} projeto${projects.length === 1 ? "" : "s"} cadastrado${projects.length === 1 ? "" : "s"}.`
      : "Cadastre um diretório existente dentro das raízes permitidas.";
    renderProjects(projects);
  } catch (error) {
    $("projects-status").textContent = error.message;
    setTableMessage("project-rows", 6, "Falha ao consultar projetos.");
  }
}

function renderProjects(projects) {
  if (!projects.length) {
    setTableMessage("project-rows", 6, "Nenhum projeto cadastrado.");
    return;
  }
  const rows = projects.map((project) => {
    const row = document.createElement("tr");
    const actions = document.createElement("td");
    actions.className = "align-right";
    const buttons = document.createElement("div");
    buttons.className = "row-actions";
    buttons.append(
      actionButton("Open with OpenCode", () => openAiFor("project", project.id, project.name, project.path), "ai"),
      actionButton("Remover", () => removeProject(project), "danger"),
    );
    actions.append(buttons);
    const gitState = project.clean === true
      ? "Clean"
      : project.clean === false
        ? `${project.modified_files} modified`
        : "No Git";
    row.append(
      tableCell(project.name, "primary-cell"),
      tableCell(project.branch || "—"),
      tableCell(gitState),
      tableCell(project.language || "—"),
      tableCell(project.path, "secondary-cell"),
      actions,
    );
    return row;
  });
  $("project-rows").replaceChildren(...rows);
}

async function registerProject(event) {
  event.preventDefault();
  const button = event.currentTarget.querySelector("button[type=submit]");
  button.disabled = true;
  try {
    await request("/api/v1/projects", {
      method: "POST",
      body: JSON.stringify({ name: $("project-name").value, path: $("project-path").value }),
    });
    $("project-name").value = "";
    $("project-path").value = "";
    toast("Projeto cadastrado; nenhum arquivo foi modificado.");
    await loadProjects();
  } catch (error) { toast(error.message, true); }
  finally { button.disabled = false; }
}

async function removeProject(project) {
  if (!window.confirm(`Remover apenas o cadastro de ${project.name}? Os arquivos não serão apagados.`)) return;
  try {
    await request(`/api/v1/projects/${encodeURIComponent(project.id)}`, { method: "DELETE" });
    toast("Cadastro removido; arquivos preservados.");
    await loadProjects();
  } catch (error) { toast(error.message, true); }
}

async function loadAudit() {
  setTableMessage("audit-rows", 7, "Carregando trilha de auditoria…");
  try {
    const events = await request("/api/v1/audit?limit=200");
    if (!events.length) {
      setTableMessage("audit-rows", 7, "Nenhuma operação modificadora registrada.");
      return;
    }
    const rows = events.map((event) => {
      const row = document.createElement("tr");
      row.append(
        tableCell(new Date(event.created_at * 1000).toLocaleString()),
        tableCell(event.actor_name, "primary-cell"),
        tableCell(event.origin),
        tableCell(event.action),
        tableCell(event.target, "secondary-cell"),
        statusCell(event.result, event.result),
        tableCell(`${event.duration_ms} ms`),
      );
      return row;
    });
    $("audit-rows").replaceChildren(...rows);
  } catch (error) { setTableMessage("audit-rows", 7, error.message); }
}

async function loadDoctor() {
  $("doctor-summary").textContent = "Executando verificações read-only…";
  const pending = document.createElement("div");
  pending.className = "doctor-empty";
  pending.textContent = "Coletando evidências do host…";
  $("doctor-findings").replaceChildren(pending);
  try {
    const report = await request("/api/v1/doctor");
    $("doctor-summary").textContent = `${report.issues} issue(s) · estado ${report.overall} · ${new Date(report.checked_at * 1000).toLocaleString()}`;
    const cards = report.findings.map((finding) => {
      const card = document.createElement("article");
      card.className = `finding ${finding.severity}`;
      const header = document.createElement("div");
      header.className = "finding-header";
      const title = document.createElement("h3");
      title.textContent = finding.title;
      const badge = document.createElement("span");
      badge.className = "finding-badge";
      badge.textContent = finding.severity;
      header.append(title, badge);
      const details = document.createElement("p");
      details.textContent = finding.details;
      const confidence = document.createElement("span");
      confidence.className = "confidence";
      confidence.textContent = `CONFIDENCE ${(finding.confidence * 100).toFixed(0)}% · ${finding.category}`;
      card.append(header, details, confidence);
      return card;
    });
    $("doctor-findings").replaceChildren(...cards);
  } catch (error) {
    pending.textContent = error.message;
  }
}

function tableCell(text, className = "") {
  const cell = document.createElement("td");
  cell.className = className;
  cell.textContent = String(text);
  return cell;
}

function statusCell(stateName, details) {
  const cell = document.createElement("td");
  const status = document.createElement("span");
  status.className = `status-pill ${String(stateName).toLocaleLowerCase()}`;
  status.textContent = details;
  cell.append(status);
  return cell;
}

function actionButton(label, handler, kind = "") {
  const button = document.createElement("button");
  button.className = `action-button ${kind}`.trim();
  button.type = "button";
  button.textContent = label;
  button.addEventListener("click", handler);
  return button;
}

function setTableMessage(id, columns, message) {
  const row = document.createElement("tr");
  const cell = document.createElement("td");
  cell.colSpan = columns;
  cell.className = "table-empty";
  cell.textContent = message;
  row.append(cell);
  $(id).replaceChildren(row);
}

function openLogDialog(title, source) {
  $("log-title").textContent = title;
  $("log-source").textContent = source;
  $("log-output").textContent = "Carregando…";
  $("log-dialog").showModal();
}

function openAiFor(kind, id, label, projectPath = null) {
  state.aiContext = { kind, target: id, label: `${kind} · ${label || id}` };
  $("ai-context").textContent = state.aiContext.label;
  if (projectPath) $("ai-project").value = projectPath;
  document.querySelector('[data-page="ai"]').click();
  toast(`Contexto preparado: ${kind} ${label || id}.`);
}

async function loadAi() {
  try {
    const [status, sessions] = await Promise.all([
      request("/api/v1/opencode/status"),
      request("/api/v1/opencode/sessions"),
    ]);
    renderAiStatus(status);
    renderAiSessions(sessions);
    ensureAiEventStream();
  } catch (error) { toast(error.message, true); }
}

function renderAiStatus(status) {
  state.aiMode = status.permission_mode;
  $("ai-status").textContent = titleCase(status.phase);
  $("ai-memory").textContent = status.phase === "sleeping" ? "~0 B" : bytes(status.memory_bytes);
  $("ai-version").textContent = status.version || (status.installed ? "installed" : "not installed");
  $("ai-timeout").textContent = `${status.idle_timeout_seconds}s`;
  $("start-ai").disabled = status.phase === "starting";
  $("stop-ai").disabled = status.phase === "sleeping";
  if (status.project_path) $("ai-project").value = status.project_path;
  setSelectedAiMode(status.permission_mode);
  $("unrestricted-warning").hidden = status.permission_mode !== "unrestricted";
  $("chat-hint").textContent = `${modeLabel(status.permission_mode)} · context automático`;
  if (status.error) toast(status.error, true);
}

function renderAiSessions(sessions) {
  if (!sessions.length) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "No sessions yet.";
    $("ai-sessions").replaceChildren(empty);
    return;
  }
  const buttons = sessions.map((session) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `session-item${state.aiSession && state.aiSession.id === session.id ? " active" : ""}`;
    const title = document.createElement("strong");
    title.textContent = session.title;
    const metadata = document.createElement("small");
    metadata.textContent = `${session.permission_mode} · ${new Date(session.last_active_at * 1000).toLocaleString()}`;
    button.append(title, metadata);
    button.addEventListener("click", () => {
      state.aiSession = session;
      renderAiSessions(sessions);
      setSelectedAiMode(session.permission_mode);
      $("ai-project").value = session.project_path || "";
      addChatMessage("agent", `Session selected: ${session.title}`);
    });
    return button;
  });
  $("ai-sessions").replaceChildren(...buttons);
}

function selectedAiMode() {
  return document.querySelector('input[name="ai-mode"]:checked').value;
}

function setSelectedAiMode(mode) {
  const radio = document.querySelector(`input[name="ai-mode"][value="${mode}"]`);
  if (radio) radio.checked = true;
}

async function confirmationFor(mode) {
  if (mode !== "unrestricted") return null;
  if (state.unrestrictedConfirmed) return "I understand OpenCode will have full control";
  const phrase = window.prompt('Digite exatamente "I understand OpenCode will have full control" para ativar controle irrestrito:');
  if (phrase !== "I understand OpenCode will have full control") throw new Error("Confirmação irrestrita não corresponde.");
  return phrase;
}

async function changeAiMode(mode) {
  const previous = state.aiMode;
  try {
    const confirmation = await confirmationFor(mode);
    await request("/api/v1/opencode/mode", {
      method: "POST",
      body: JSON.stringify({ permission_mode: mode, confirmation }),
    });
    state.aiMode = mode;
    state.unrestrictedConfirmed = mode === "unrestricted";
    state.aiSession = null;
    clearPendingPermissions();
    $("unrestricted-warning").hidden = mode !== "unrestricted";
    $("chat-hint").textContent = `${modeLabel(mode)} · context automático`;
    renderAiStatus(await request("/api/v1/opencode/status"));
    toast(`AI permission mode: ${modeLabel(mode)}. Agent stopped to enforce the new policy.`);
  } catch (error) {
    setSelectedAiMode(previous);
    toast(error.message, true);
  }
}

async function startAi() {
  const mode = selectedAiMode();
  try {
    const confirmation = await confirmationFor(mode);
    $("ai-status").textContent = "Starting";
    const project = $("ai-project").value.trim();
    const status = await request("/api/v1/opencode/start", {
      method: "POST",
      body: JSON.stringify({ project_path: project || null, permission_mode: mode, confirmation }),
    });
    if (mode === "unrestricted") state.unrestrictedConfirmed = true;
    renderAiStatus(status);
    toast("OpenCode ready on loopback.");
  } catch (error) { $("ai-status").textContent = "Error"; toast(error.message, true); }
}

async function stopAi() {
  try {
    await request("/api/v1/opencode/stop", { method: "POST", body: "{}" });
    clearPendingPermissions();
    renderAiStatus(await request("/api/v1/opencode/status"));
    toast("OpenCode stopped; session state remains persisted.");
  } catch (error) { toast(error.message, true); }
}

async function createAiSession() {
  const mode = selectedAiMode();
  try {
    const confirmation = await confirmationFor(mode);
    const project = $("ai-project").value.trim();
    const session = await request("/api/v1/opencode/sessions", {
      method: "POST",
      body: JSON.stringify({
        title: state.aiContext.label,
        project_path: project || null,
        permission_mode: mode,
        confirmation,
      }),
    });
    if (mode === "unrestricted") state.unrestrictedConfirmed = true;
    state.aiSession = session;
    addChatMessage("agent", `Session ready in ${session.project_path}. Permission mode: ${session.permission_mode}.`);
    await loadAi();
    return session;
  } catch (error) { toast(error.message, true); throw error; }
}

async function sendChat(event) {
  event.preventDefault();
  const input = $("chat-prompt");
  const message = input.value.trim();
  if (!message) return;
  const button = event.currentTarget.querySelector("button[type=submit]");
  button.disabled = true;
  addChatMessage("user", message);
  input.value = "";
  try {
    const session = state.aiSession || await createAiSession();
    const response = await request(`/api/v1/opencode/sessions/${encodeURIComponent(session.id)}/messages`, {
      method: "POST",
      body: JSON.stringify({
        message,
        context: { kind: state.aiContext.kind, target: state.aiContext.target },
      }),
    });
    const text = Array.isArray(response.parts)
      ? response.parts.filter((part) => part.type === "text").map((part) => part.text).join("\n")
      : "";
    addChatMessage("agent", text || JSON.stringify(response, null, 2));
  } catch (error) { addChatMessage("agent", `Error: ${error.message}`); }
  finally { button.disabled = false; input.focus(); }
}

function addChatMessage(kind, text) {
  const item = document.createElement("div");
  item.className = kind === "user" ? "user-message" : "agent-message";
  const author = document.createElement("span");
  author.textContent = kind === "user" ? (state.user ? state.user.username : "User") : "OpenCode";
  const content = document.createElement("p");
  content.textContent = text;
  item.append(author, content);
  $("chat-messages").append(item);
  $("chat-messages").scrollTop = $("chat-messages").scrollHeight;
}

function ensureAiEventStream() {
  if (state.aiStream) return;
  state.aiStream = new EventSource("/api/v1/opencode/events");
  const permissionHandler = (event) => {
    try {
      const value = JSON.parse(event.data);
      const details = value.properties || {};
      const id = details.id || details.requestID;
      if (id && !state.pendingPermissions.some((pending) => (pending.id || pending.requestID) === id)) {
        state.pendingPermissions.push(details);
      }
      renderPendingPermission();
    } catch (_) { /* ignore malformed events */ }
  };
  state.aiStream.addEventListener("permission.asked", permissionHandler);
  state.aiStream.addEventListener("permission.v2.asked", permissionHandler);
  state.aiStream.addEventListener("session.status", (event) => {
    try {
      const value = JSON.parse(event.data);
      $("ai-status").textContent = titleCase(value.properties.status.type || "ready");
    } catch (_) { /* ignore malformed events */ }
  });
}

function renderPendingPermission() {
  const details = state.pendingPermissions[0];
  if (!details) {
    $("approval-card").hidden = true;
    return;
  }
  $("approval-title").textContent = `OpenCode requests: ${details.permission || details.action || "tool action"}`;
  $("approval-details").textContent = JSON.stringify({
    queued: state.pendingPermissions.length,
    patterns: details.patterns,
    resources: details.resources,
    metadata: details.metadata,
  }, null, 2);
  $("approval-card").hidden = false;
}

function clearPendingPermissions() {
  state.pendingPermissions = [];
  $("approval-card").hidden = true;
}

async function answerPermission(reply) {
  const pending = state.pendingPermissions[0];
  const id = pending && (pending.id || pending.requestID);
  if (!id) return;
  try {
    await request(`/api/v1/opencode/permissions/${encodeURIComponent(id)}`, {
      method: "POST",
      body: JSON.stringify({ reply, message: null }),
    });
    state.pendingPermissions.shift();
    renderPendingPermission();
    toast(reply === "reject" ? "OpenCode action rejected." : "OpenCode action approved once.");
  } catch (error) { toast(error.message, true); }
}

function titleCase(value) { return String(value || "").replaceAll("_", " ").replace(/^./, (letter) => letter.toUpperCase()); }
function modeLabel(mode) { return mode === "read_only" ? "Read Only" : mode === "approval" ? "Require Approval" : "Unrestricted"; }

$("refresh-docker").addEventListener("click", loadDocker);
$("refresh-services").addEventListener("click", loadServices);
$("refresh-log-sources").addEventListener("click", loadLogs);
$("start-log-stream").addEventListener("click", startLogStream);
$("pause-log-stream").addEventListener("click", toggleLogPause);
$("clear-stream-logs").addEventListener("click", clearStreamLogs);
$("export-stream-logs").addEventListener("click", exportStreamLogs);
$("stream-log-source").addEventListener("change", () => {
  stopLogStream(false);
  clearStreamLogs();
  renderLogTargets();
});
$("stream-log-target").addEventListener("change", () => {
  stopLogStream(false);
  clearStreamLogs();
});
$("stream-log-filter").addEventListener("input", renderStreamLogs);
$("stream-log-level").addEventListener("change", renderStreamLogs);
$("refresh-audit").addEventListener("click", loadAudit);
$("project-form").addEventListener("submit", registerProject);
$("run-doctor").addEventListener("click", loadDoctor);
$("doctor-ai").addEventListener("click", () => openAiFor("doctor", "latest", "Server Doctor"));
$("start-ai").addEventListener("click", startAi);
$("stop-ai").addEventListener("click", stopAi);
$("new-ai-session").addEventListener("click", () => { createAiSession().catch(() => {}); });
$("chat-form").addEventListener("submit", sendChat);
$("approve-permission").addEventListener("click", () => answerPermission("once"));
$("reject-permission").addEventListener("click", () => answerPermission("reject"));
document.querySelectorAll('input[name="ai-mode"]').forEach((radio) => {
  radio.addEventListener("change", () => { if (radio.checked) changeAiMode(radio.value); });
});
$("service-filter").addEventListener("input", renderServices);
$("close-logs").addEventListener("click", () => $("log-dialog").close());
$("copy-logs").addEventListener("click", async () => {
  try { await navigator.clipboard.writeText($("log-output").textContent); toast("Logs copiados."); }
  catch (_) { toast("O navegador bloqueou a cópia.", true); }
});

window.addEventListener("resize", drawHistory);
boot();
