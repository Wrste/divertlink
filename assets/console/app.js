/* DivertLink console — vanilla JS SPA (no build step, no dependencies).
 *
 * Talks to the local REST API of `divertlink console`:
 *   GET  /api/state, /api/profiles, /api/profiles/:id/status|config|logs/history
 *   POST /api/profiles, /api/profiles/:id/{start,stop,restart}
 *   PUT  /api/profiles/:id/config, POST /api/init, /api/probe, /api/token ...
 * Mutating requests carry `x-divertlink-console: 1` (CSRF guard).
 */
"use strict";

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));

const state = {
  profiles: [],
  selected: null,
  detail: null,
  configDirty: false,
  refreshing: false,
  logPaused: false,
  autoscroll: true,
  eventSource: null,
  token: localStorage.getItem("divertlink.console.token") || "",
};

/* ------------------------------------------------------------------ utils */

function api(path, options = {}) {
  const opts = Object.assign({ method: "GET" }, options);
  opts.headers = Object.assign(
    { "x-divertlink-console": "1", "content-type": "application/json" },
    opts.headers || {}
  );
  if (state.token) opts.headers["x-console-token"] = state.token;
  return fetch(path, opts).then(async (res) => {
    const text = await res.text();
    let body = null;
    try { body = text ? JSON.parse(text) : null; } catch { body = { raw: text }; }
    if (res.status === 401) {
      // Token-protected console: ask once, store locally, then retry.
      const entered = window.prompt("该控制台需要访问令牌（--token）：", state.token || "");
      if (entered !== null) {
        state.token = entered.trim();
        localStorage.setItem("divertlink.console.token", state.token);
        return api(path, options);
      }
    }
    if (!res.ok || (body && body.ok === false)) {
      const message = (body && body.error && body.error.message) || res.statusText || "请求失败";
      throw new Error(message);
    }
    return body ? body.data : null;
  });
}

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function toast(message, kind = "") {
  const el = document.createElement("div");
  el.className = "toast " + kind;
  el.textContent = message;
  $("#toasts").appendChild(el);
  setTimeout(() => el.remove(), 4200);
}

function fmtBytes(n) {
  if (!n) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  return (i === 0 ? v : v.toFixed(1)) + " " + units[i];
}

function fmtUptime(seconds) {
  if (!seconds || seconds < 0) return "—";
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (d) return `${d}天${h}小时`;
  if (h) return `${h}小时${m}分`;
  if (m) return `${m}分${s}秒`;
  return `${s}秒`;
}

function fmtTime(unix) {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  return d.toLocaleString("zh-CN", { hour12: false });
}

function clock(unix) {
  const d = new Date(unix * 1000);
  return d.toLocaleTimeString("zh-CN", { hour12: false });
}

function roleLabel(role) {
  return role === "server" ? "Gateway" : "Agent";
}

function statusLabel(status) {
  switch (status) {
    case "running": return "运行中";
    case "stopping": return "停止中";
    case "failed": return "异常退出";
    case "starting": return "启动中";
    default: return "未运行";
  }
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("已复制", "ok");
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
    toast("已复制", "ok");
  }
}

/* --------------------------------------------------------------- rendering */

function currentProfile() {
  return state.profiles.find((p) => p.id === state.selected) || null;
}

function renderProfiles() {
  const list = $("#profile-list");
  const filter = ($("#search").value || "").trim().toLowerCase();
  list.innerHTML = "";
  const profiles = state.profiles.filter((p) => {
    if (!filter) return true;
    return (p.name + " " + p.config + " " + roleLabel(p.role)).toLowerCase().includes(filter);
  });
  if (!profiles.length) {
    list.innerHTML = `<div class="hint" style="padding:10px 6px">${state.profiles.length ? "没有匹配的配置" : "还没有配置，点击下方「新建项目」"}</div>`;
    return;
  }
  for (const p of profiles) {
    const btn = document.createElement("button");
    btn.className = "profile" + (p.id === state.selected ? " active" : "");
    btn.innerHTML = `
      <span class="dot ${p.status ? p.status.state : "stopped"}"></span>
      <span class="meta">
        <span class="name">${escapeHtml(p.name)}</span>
        <span class="sub">${escapeHtml(p.summary && p.summary.server ? p.summary.server : (p.config || ""))}</span>
      </span>
      <span class="role">${roleLabel(p.role)}</span>`;
    btn.addEventListener("click", () => selectProfile(p.id));
    list.appendChild(btn);
  }
}

function renderHeader() {
  const p = currentProfile();
  const has = Boolean(p);
  $("#profile-title").textContent = has ? p.name : "未选择配置";
  let subtitle = "从左侧选择或新建一个配置";
  if (has) {
    const s = p.summary || {};
    if (p.role === "agent") {
      subtitle = `Agent → ${s.server || "?"} → 本地 ${s.target || "?"}`;
    } else {
      subtitle = `Gateway ${s.name || ""} · 隧道 ${s.tunnels || 0} · API Key ${s.api_keys || 0}`;
    }
  }
  $("#profile-subtitle").textContent = subtitle;
  const st = has && p.status ? p.status.state : "stopped";
  const pill = $("#status-pill");
  pill.dataset.state = st;
  pill.querySelector("span").textContent = statusLabel(st);
  $("#btn-start").disabled = !has || st === "running" || st === "stopping";
  $("#btn-stop").disabled = !has || st === "stopped" || st === "failed";
  $("#btn-restart").disabled = !has;
  $("#btn-delete").disabled = !has;
  $("#empty-state").hidden = has;
  $("#cards").hidden = !has;
}

function renderOverview() {
  const p = currentProfile();
  if (!p) return;
  $("#overview-role").textContent = roleLabel(p.role);
  const kv = $("#overview-kv");
  const s = p.summary || {};
  const rows = [];
  rows.push(["配置文件", `<code>${escapeHtml(p.config || "")}</code>`]);
  rows.push(["状态", `<span data-state="${p.status.state}">${statusLabel(p.status.state)}</span>`]);
  if (p.status.pid) rows.push(["PID", String(p.status.pid)]);
  if (p.status.started_unix) {
    rows.push(["已运行", fmtUptime(Math.floor(Date.now() / 1000) - p.status.started_unix)]);
  }
  if (p.status.last_error) rows.push(["错误", `<span class="bad">${escapeHtml(p.status.last_error)}</span>`]);
  if (p.role === "agent") {
    rows.push(["网关", `<code>${escapeHtml(s.server || "—")}</code>`]);
    rows.push(["隧道 ID", `<code>${escapeHtml(s.tunnel_id || "—")}</code>`]);
    rows.push(["本地目标", `<code>${escapeHtml(s.target || "—")}</code>`]);
    rows.push(["心跳/重连", "应用层心跳·指数退避"]);
  } else {
    rows.push(["实例名", escapeHtml(s.name || "—")]);
    if (s.listeners) {
      for (const [name, addr] of Object.entries(s.listeners)) {
        rows.push([name, `<code>${escapeHtml(addr)}</code>`]);
      }
    }
    rows.push(["隧道 / Key", `${s.tunnels || 0} / ${s.api_keys || 0}`]);
    if (s.mounts && s.mounts.length) rows.push(["路由", s.mounts.map((m) => `<code>${escapeHtml(m)}</code>`).join(" ")]);
    rows.push(["入口鉴权", s.auth_enabled ? "已启用 API Key" : "未启用（不推荐）"]);
  }
  kv.innerHTML = rows.map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${v}</dd>`).join("");
}

function renderLive() {
  const p = currentProfile();
  if (!p) return;
  const detail = state.detail || {};
  const admin = detail.admin || null;
  const metrics = $("#live-metrics");
  const tunnels = $("#live-tunnels");
  if (p.role !== "server") {
    metrics.innerHTML = `
      <div class="metric"><div class="v">${p.status.state === "running" ? "在线" : "离线"}</div><div class="k">进程状态</div></div>
      <div class="metric"><div class="v">${p.status.pid || "—"}</div><div class="k">PID</div></div>
      <div class="metric"><div class="v">${p.status.started_unix ? fmtUptime(Math.floor(Date.now() / 1000) - p.status.started_unix) : "—"}</div><div class="k">运行时长</div></div>
      <div class="metric"><div class="v">${p.status.restarts || 0}</div><div class="k">重启次数</div></div>`;
    tunnels.innerHTML = `<div class="hint">Agent 进程由控制台托管；网关侧的隧道连接状态请查看网关配置的「实时状态」。</div>`;
    $("#live-refreshed").textContent = "";
    return;
  }
  const t = (admin && admin.tunnels) || [];
  const connected = t.filter((x) => x.connected).length;
  const stats = t.reduce(
    (acc, x) => {
      acc.req += x.stats.requests_total || 0;
      acc.err += x.stats.errors_total || 0;
      acc.in += x.stats.bytes_in_total || 0;
      acc.out += x.stats.bytes_out_total || 0;
      return acc;
    },
    { req: 0, err: 0, in: 0, out: 0 }
  );
  metrics.innerHTML = `
    <div class="metric"><div class="v">${connected}/${t.length}</div><div class="k">在线隧道</div></div>
    <div class="metric"><div class="v">${stats.req}</div><div class="k">请求总数</div></div>
    <div class="metric"><div class="v">${fmtBytes(stats.in + stats.out)}</div><div class="k">流量</div></div>
    <div class="metric"><div class="v">${stats.err}</div><div class="k">失败请求</div></div>`;
  tunnels.innerHTML = t
    .map(
      (x) => `
      <div class="tunnel">
        <span class="dot ${x.connected ? "on" : ""}"></span>
        <strong>${escapeHtml(x.id)}</strong>
        <code>${escapeHtml(x.mount || (x.hosts && x.hosts[0]) || "host/default")}</code>
        <span class="muted" style="margin-left:auto">${x.connected ? (x.session ? "agent " + escapeHtml(x.session.agent_version || "") : "在线") : "离线"}</span>
      </div>`
    )
    .join("") || `<div class="hint">尚未配置隧道</div>`;
  $("#live-refreshed").textContent = admin ? "每 5 秒刷新" : "进程未运行或管理端点不可达";
}

function renderProbe(report) {
  const box = $("#probe-result");
  if (!report) {
    box.className = "probe muted";
    box.textContent = "检测 TLS 握手、延迟、证书有效期与 SPKI pin。";
    return;
  }
  box.className = "probe";
  const cert = report.cert || {};
  const rows = [
    ["结果", report.ok ? '<span class="ok">连接成功</span>' : `<span class="bad">失败：${escapeHtml(report.error || "未知错误")}</span>`],
    ["地址", `<code>${escapeHtml(report.address || "")}</code>`],
    ["延迟", `${report.latency_ms} ms`],
    ["TLS", `${escapeHtml(report.tls_version || "—")} · ${escapeHtml(report.cipher || "—")}`],
    ["ALPN", `<code>${escapeHtml(report.alpn || "—")}</code>`],
  ];
  if (cert.subject) {
    rows.push(["证书", `<code>${escapeHtml(cert.subject)}</code>`]);
    rows.push(["到期", `${fmtTime(cert.not_after_unix)}（${cert.days_left} 天）`]);
    if (cert.sans && cert.sans.length) rows.push(["SAN", `<code>${escapeHtml(cert.sans.slice(0, 6).join(", "))}</code>`]);
    rows.push([
      "SPKI pin",
      `<code>${escapeHtml(cert.spki_pin || "")}</code> <button class="btn small" data-copy="${escapeHtml(cert.spki_pin || "")}">复制</button>`,
    ]);
  }
  box.innerHTML = `<table>${rows.map(([k, v]) => `<tr><td>${escapeHtml(k)}</td><td>${v}</td></tr>`).join("")}</table>`;
  $$("[data-copy]", box).forEach((btn) => btn.addEventListener("click", () => copyText(btn.dataset.copy)));
}

function renderLogs(lines) {
  const box = $("#logs");
  if (!lines.length) {
    box.innerHTML = '<div class="log-empty">暂无日志（启动后这里会实时显示 stdout/stderr）</div>';
    return;
  }
  box.innerHTML = lines
    .map(
      (l) => `<div class="log-line ${escapeHtml(l.stream)}">
        <span class="t">${clock(l.at_unix)}</span>
        <span class="s">${escapeHtml(l.stream)}</span>
        <span class="m">${escapeHtml(l.line)}</span>
      </div>`
    )
    .join("");
  if (state.autoscroll) box.scrollTop = box.scrollHeight;
}

function appendLog(line) {
  if (state.logPaused) return;
  const box = $("#logs");
  if (box.querySelector(".log-empty")) box.innerHTML = "";
  const el = document.createElement("div");
  el.className = "log-line " + line.stream;
  el.innerHTML = `<span class="t">${clock(line.at_unix)}</span><span class="s">${escapeHtml(line.stream)}</span><span class="m">${escapeHtml(line.line)}</span>`;
  box.appendChild(el);
  while (box.childElementCount > 1500) box.removeChild(box.firstChild);
  if (state.autoscroll) box.scrollTop = box.scrollHeight;
}

/* ------------------------------------------------------------------ data */

async function refreshState() {
  if (state.refreshing) return;
  state.refreshing = true;
  try {
    const data = await api("/api/state");
    state.profiles = data.profiles || [];
    if (!state.selected && data.selected) state.selected = data.selected;
    if (state.selected && !state.profiles.some((p) => p.id === state.selected)) {
      state.selected = state.profiles.length ? state.profiles[0].id : null;
    }
    renderProfiles();
    renderHeader();
    if (state.selected) renderOverview();
  } catch (err) {
    console.warn("state refresh failed", err);
  } finally {
    state.refreshing = false;
  }
}

async function loadDetail(profileId) {
  try {
    const detail = await api(`/api/profiles/${profileId}/status`);
    state.detail = detail;
    renderOverview();
    renderLive();
    if (state.selected === profileId) {
      const cfg = await api(`/api/profiles/${profileId}/config`);
      if (!state.configDirty) {
        $("#config-editor").value = cfg.content || "";
        $("#config-path").textContent = cfg.path || "";
        $("#config-msg").textContent = "";
        $("#config-msg").className = "config-msg";
      }
    }
  } catch (err) {
    // Details are best-effort: a stopped profile still renders.
    state.detail = null;
    renderLive();
  }
}

async function loadLogs(profileId) {
  try {
    const lines = await api(`/api/profiles/${profileId}/logs/history?limit=800`);
    renderLogs(lines || []);
  } catch {
    renderLogs([]);
  }
}

function startLogStream(profileId) {
  if (state.eventSource) state.eventSource.close();
  const url = `/api/logs/history?after=0`; // placeholder for capabilities probing
  void url;
  const query = state.token ? `?token=${encodeURIComponent(state.token)}` : "";
  const source = new EventSource(`/api/logs${query}`);
  state.eventSource = source;
  source.addEventListener("log", (event) => {
    try {
      const line = JSON.parse(event.data);
      if (line.profile !== profileId && line.profile !== "_console") return;
      appendLog(line);
    } catch { /* ignore malformed */ }
  });
  source.onerror = () => {
    // Browsers retry automatically; avoid spamming the console.
  };
}

async function selectProfile(id) {
  state.selected = id;
  state.configDirty = false;
  localStorage.setItem("divertlink.selected", id);
  await api("/api/select", { method: "POST", body: JSON.stringify({ id }) }).catch(() => {});
  renderProfiles();
  renderHeader();
  renderOverview();
  document.body.classList.remove("sidebar-open");
  $("#sidebar").classList.remove("open");
  await loadDetail(id);
  await loadLogs(id);
  renderProbe(null);
  startLogStream(id);
}

async function profileAction(action) {
  const p = currentProfile();
  if (!p) return;
  try {
    await api(`/api/profiles/${p.id}/${action}`, { method: "POST", body: "{}" });
    toast(`${p.name}：${action === "start" ? "已启动" : action === "stop" ? "已停止" : "已重启"}`, "ok");
    await refreshState();
    await loadDetail(p.id);
  } catch (err) {
    toast(err.message, "bad");
  }
}

/* ------------------------------------------------------------------ modals */

function openModal(id) { $(id).hidden = false; }
function closeModal(id) { $(id).hidden = true; }
function closeAllModals() { $$(".modal").forEach((m) => (m.hidden = true)); }

function showResult(title, rows) {
  $("#result-body").innerHTML = rows
    .map(
      ([k, v]) => `<div class="result-item"><span class="k">${escapeHtml(k)}</span><code>${escapeHtml(v)}</code>
        <button class="btn small" data-copy="${escapeHtml(v)}">复制</button></div>`
    )
    .join("");
  $$("[data-copy]", $("#result-body")).forEach((btn) =>
    btn.addEventListener("click", () => copyText(btn.dataset.copy))
  );
  $(".modal-head h2", $("#modal-result")).textContent = title;
  openModal("#modal-result");
}

async function submitNewProject() {
  const dir = $("#new-dir").value.trim();
  const domain = $("#new-domain").value.trim();
  const role = $("#new-role").value;
  const mtls = $("#new-mtls").checked;
  const force = $("#new-force").checked;
  const msg = $("#new-msg");
  msg.className = "modal-msg";
  msg.textContent = "";
  if (!dir) { msg.className = "modal-msg bad"; msg.textContent = "请填写项目目录"; return; }
  if (!domain) { msg.className = "modal-msg bad"; msg.textContent = "请填写域名或 IP"; return; }
  $("#btn-create").disabled = true;
  try {
    const data = await api("/api/init", {
      method: "POST",
      body: JSON.stringify({ dir, domain, role, mtls, force }),
    });
    const rows = [];
    if (data.server_toml) rows.push(["server.toml", data.server_toml]);
    if (data.agent_toml) rows.push(["agent.toml", data.agent_toml]);
    rows.push(["API Key", data.api_key]);
    rows.push(["Agent Token", data.agent_token]);
    if (data.server_pin) rows.push(["SPKI pin", data.server_pin]);
    closeModal("#modal-new");
    showResult("项目已生成", rows);
    await refreshState();
    if (data.profiles && data.profiles.length) {
      await selectProfile(data.profiles[0].id);
    }
  } catch (err) {
    msg.className = "modal-msg bad";
    msg.textContent = err.message;
  } finally {
    $("#btn-create").disabled = false;
  }
}

async function submitAddProfile() {
  const name = $("#add-name").value.trim();
  const config = $("#add-path").value.trim();
  const autostart = $("#add-autostart").checked;
  const msg = $("#add-msg");
  msg.className = "modal-msg";
  msg.textContent = "";
  if (!name) { msg.className = "modal-msg bad"; msg.textContent = "请填写显示名称"; return; }
  if (!config) { msg.className = "modal-msg bad"; msg.textContent = "请填写配置路径"; return; }
  try {
    const profile = await api("/api/profiles", {
      method: "POST",
      body: JSON.stringify({ name, config, autostart }),
    });
    closeModal("#modal-add");
    toast("已添加配置", "ok");
    await refreshState();
    await selectProfile(profile.id);
  } catch (err) {
    msg.className = "modal-msg bad";
    msg.textContent = err.message;
  }
}

/* ------------------------------------------------------------------ actions */

async function saveConfig() {
  const p = currentProfile();
  if (!p) return;
  const content = $("#config-editor").value;
  const msg = $("#config-msg");
  try {
    const data = await api(`/api/profiles/${p.id}/config`, {
      method: "PUT",
      body: JSON.stringify({ content }),
    });
    state.configDirty = false;
    msg.className = "config-msg ok";
    msg.textContent = "已保存（旧版本备份为 .toml.bak）";
    toast("配置已保存", "ok");
    await refreshState();
    state.detail = data;
  } catch (err) {
    msg.className = "config-msg bad";
    msg.textContent = "保存失败：" + err.message;
  }
}

async function generateToken() {
  const p = currentProfile();
  if (!p) return;
  try {
    const prefix = p.role === "agent" ? "agt_" : "dlk_";
    const data = await api("/api/token", {
      method: "POST",
      body: JSON.stringify({ prefix, bytes: 24 }),
    });
    await copyText(data.token);
    if (p.role === "agent") {
      toast("已生成 agent token 并复制，请同时替换网关端 [[tunnels]].token", "ok");
    } else {
      toast("已生成 API Key 并复制，可加入 [[api_keys]]", "ok");
    }
  } catch (err) {
    toast(err.message, "bad");
  }
}

async function probeSelected() {
  const p = currentProfile();
  if (!p) return;
  $("#btn-probe").disabled = true;
  $("#probe-result").className = "probe muted";
  $("#probe-result").textContent = "探测中…";
  try {
    const report = await api("/api/probe", {
      method: "POST",
      body: JSON.stringify({ profile: p.id }),
    });
    renderProbe(report);
  } catch (err) {
    renderProbe({ ok: false, error: err.message, latency_ms: 0 });
  } finally {
    $("#btn-probe").disabled = false;
  }
}

async function deleteSelected() {
  const p = currentProfile();
  if (!p) return;
  if (!confirm(`删除配置「${p.name}」？（只从列表移除，不删除磁盘文件）`)) return;
  try {
    await api(`/api/profiles/${p.id}`, { method: "DELETE" });
    toast("已删除", "ok");
    state.selected = null;
    state.detail = null;
    await refreshState();
    closeAllModals();
    if (state.profiles.length) await selectProfile(state.profiles[0].id);
    else {
      $("#cards").hidden = true;
      $("#empty-state").hidden = false;
      renderHeader();
    }
  } catch (err) {
    toast(err.message, "bad");
  }
}

async function exportProfiles() {
  try {
    const data = await api("/api/export");
    const blob = new Blob([data.profiles], { type: "application/json" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "divertlink-profiles.json";
    a.click();
    URL.revokeObjectURL(a.href);
  } catch (err) {
    toast(err.message, "bad");
  }
}

function importProfiles() {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = "application/json,.json";
  input.addEventListener("change", async () => {
    const file = input.files && input.files[0];
    if (!file) return;
    const text = await file.text();
    try {
      const result = await api("/api/import", {
        method: "POST",
        body: JSON.stringify({ profiles: text }),
      });
      toast(`已导入 ${result.imported} 个配置`, "ok");
      await refreshState();
    } catch (err) {
      toast(err.message, "bad");
    }
  });
  input.click();
}

function toggleTheme() {
  const app = $("#app");
  const current =
    localStorage.getItem("divertlink.theme") ||
    (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  const next = current === "light" ? "dark" : "light";
  localStorage.setItem("divertlink.theme", next);
  app.dataset.theme = next;
  document.documentElement.dataset.theme = next;
}

function applyTheme() {
  const saved = localStorage.getItem("divertlink.theme");
  if (saved) {
    $("#app").dataset.theme = saved;
    document.documentElement.dataset.theme = saved;
  }
}

/* ------------------------------------------------------------------- boot */

function bind() {
  $("#btn-new").addEventListener("click", () => openModal("#modal-new"));
  $("#btn-new-2").addEventListener("click", () => openModal("#modal-new"));
  $("#btn-add").addEventListener("click", () => openModal("#modal-add"));
  $("#btn-add-2").addEventListener("click", () => openModal("#modal-add"));
  $("#btn-create").addEventListener("click", submitNewProject);
  $("#btn-add-save").addEventListener("click", submitAddProfile);
  $("#btn-start").addEventListener("click", () => profileAction("start"));
  $("#btn-stop").addEventListener("click", () => profileAction("stop"));
  $("#btn-restart").addEventListener("click", () => profileAction("restart"));
  $("#btn-delete").addEventListener("click", deleteSelected);
  $("#btn-save-config").addEventListener("click", saveConfig);
  $("#btn-token").addEventListener("click", generateToken);
  $("#btn-probe").addEventListener("click", probeSelected);
  $("#btn-clear-logs").addEventListener("click", () => renderLogs([]));
  $("#btn-import").addEventListener("click", importProfiles);
  $("#btn-export").addEventListener("click", exportProfiles);
  $("#btn-theme").addEventListener("click", toggleTheme);
  $("#log-autoscroll").addEventListener("change", (e) => (state.autoscroll = e.target.checked));
  $("#log-pause").addEventListener("change", (e) => (state.logPaused = e.target.checked));
  $("#search").addEventListener("input", renderProfiles);
  $("#config-editor").addEventListener("input", () => (state.configDirty = true));
  $("#sidebar-open").addEventListener("click", () => $("#sidebar").classList.add("open"));
  $("#sidebar-close").addEventListener("click", () => $("#sidebar").classList.remove("open"));
  $$("[data-close]").forEach((el) => el.addEventListener("click", () => closeAllModals()));
  $$(".modal").forEach((modal) =>
    modal.addEventListener("click", (e) => {
      if (e.target === modal) closeAllModals();
    })
  );
  document.addEventListener("keydown", (e) => {
    if (e.target && ["INPUT", "TEXTAREA", "SELECT"].includes(e.target.tagName)) {
      if (e.key === "Escape") closeAllModals();
      return;
    }
    if (e.key === "Escape") closeAllModals();
    if (e.key === "/") { e.preventDefault(); $("#search").focus(); }
    if (e.key === "n") { e.preventDefault(); openModal("#modal-new"); }
    if (e.key === "r") { e.preventDefault(); profileAction("restart"); }
    if (e.key === "p") { e.preventDefault(); probeSelected(); }
  });

  // PWA install button (appears when the browser supports it).
  window.addEventListener("beforeinstallprompt", (event) => {
    event.preventDefault();
    const install = document.createElement("button");
    install.className = "btn";
    install.textContent = "安装为桌面应用";
    install.addEventListener("click", async () => {
      event.prompt();
      await event.userChoice;
      install.remove();
    });
    $(".topbar-actions").prepend(install);
  });
}

async function boot() {
  applyTheme();
  bind();
  try {
    await refreshState();
    const saved = localStorage.getItem("divertlink.selected");
    const target = (saved && state.profiles.find((p) => p.id === saved)) || state.profiles[0];
    if (target) await selectProfile(target.id);
  } catch (err) {
    toast("无法连接控制台后端：" + err.message, "bad");
  }
  // Periodic refresh keeps status pills and server metrics live.
  setInterval(async () => {
    await refreshState();
    if (state.selected) {
      const p = currentProfile();
      if (p && p.role === "server") await loadDetail(state.selected);
    }
  }, 5000);

  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").catch(() => {});
  }
}

document.addEventListener("DOMContentLoaded", boot);
