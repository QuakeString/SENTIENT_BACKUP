// Frontend for the SENTIENT Backup & Restore desktop app. Uses Tauri's global
// `invoke` / `Channel` (withGlobalTauri = true) — no bundler.

const invoke = window.__TAURI__?.core?.invoke;
const Channel = window.__TAURI__?.core?.Channel;

const $ = (id) => document.getElementById(id);
let categories = []; // last inspect result
let restoreFile = null; // chosen archive path
let profilesList = []; // saved connection profiles

// ---- Theme: follow OS by default, manual override persisted in localStorage --
function effectiveDark() {
  const t = localStorage.getItem("theme");
  if (t === "dark") return true;
  if (t === "light") return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}
function refreshThemeIcon() {
  const use = $("themeIcon");
  if (use) use.setAttribute("href", effectiveDark() ? "#i-sun" : "#i-moon");
}
function currentThemeMode() { return localStorage.getItem("theme") || "auto"; }
function syncThemeRadios() {
  const m = currentThemeMode();
  document.querySelectorAll('input[name="themeMode"]').forEach((r) => (r.checked = r.value === m));
}
function applyThemeMode(mode) {
  if (mode === "auto") {
    localStorage.removeItem("theme");
    document.documentElement.removeAttribute("data-theme");
  } else {
    localStorage.setItem("theme", mode);
    document.documentElement.setAttribute("data-theme", mode);
  }
  refreshThemeIcon();
  syncThemeRadios();
}
function initTheme() {
  const t = localStorage.getItem("theme");
  if (t) document.documentElement.setAttribute("data-theme", t);
  refreshThemeIcon();
  syncThemeRadios();
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (!localStorage.getItem("theme")) refreshThemeIcon();
  });
}
function toggleTheme() { applyThemeMode(effectiveDark() ? "light" : "dark"); }

function humanBytes(b) {
  const u = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}

function conn() {
  return {
    host: $("host").value,
    port: Number($("port").value),
    dbname: $("dbname").value,
    user: $("user").value,
    password: $("password").value,
  };
}

// ---- Reusable progress widget (animated bar + collapsible verbose log) -------
function ProgressView(prefix) {
  const el = (s) => $(prefix + s);
  const bar = () => el("Bar");
  const fill = () => bar().querySelector(".fill");

  el("Toggle").addEventListener("click", () => {
    const log = el("Log");
    const open = log.style.display !== "none";
    log.style.display = open ? "none" : "";
    el("Toggle").textContent = (open ? "▸" : "▾") + " Details";
  });

  return {
    start() {
      el("Progress").style.display = "";
      el("Log").textContent = "";
      el("Step").textContent = "Starting…";
      bar().classList.remove("err");
      bar().classList.add("active");
      fill().style.width = "4%";
    },
    message(p) {
      if (p.type === "step") {
        el("Step").textContent = `[${p.index}/${p.total}] ${p.name}`;
        fill().style.width = Math.round((p.index / p.total) * 100) + "%";
      } else if (p.type === "log") {
        const lg = el("Log");
        lg.textContent += p.line + "\n";
        lg.scrollTop = lg.scrollHeight;
      } else if (p.type === "done") {
        el("Step").textContent = "✓ " + p.message;
        fill().style.width = "100%";
        bar().classList.remove("active");
      }
    },
    succeed() {
      bar().classList.remove("active");
      fill().style.width = "100%";
    },
    fail() {
      bar().classList.remove("active");
      bar().classList.add("err");
    },
    channel() {
      const ch = new Channel();
      ch.onmessage = (p) => this.message(p);
      return ch;
    },
  };
}
const backupProgress = ProgressView("b");
const restoreProgress = ProgressView("r");

// ---- Connection / inspect ----------------------------------------------------
function recalcTotal() {
  let bytes = 0, rows = 0, tables = 0;
  document.querySelectorAll("input.cat[type=checkbox]").forEach((cb) => {
    if (cb.checked) {
      const c = categories[Number(cb.dataset.i)];
      bytes += c.bytes; rows += c.rows; tables += c.tables.length;
    }
  });
  $("tSize").textContent = humanBytes(bytes);
  $("tRows").textContent = rows.toLocaleString();
  $("tTables").textContent = tables;
}

function syncTeleOpts() {
  const cb = document.querySelector('input.cat[data-cid="telemetry_historical"]');
  $("teleOpts").style.display = cb && cb.checked ? "" : "none";
}

function renderCategories() {
  const tbody = $("cats");
  tbody.innerHTML = "";
  categories.forEach((c, i) => {
    const tr = document.createElement("tr");
    const checked = c.default_selected ? "checked" : "";
    const disabled = c.locked ? "disabled" : "";
    tr.innerHTML = `
      <td><input class="cat" type="checkbox" data-i="${i}" data-cid="${c.id}" ${checked} ${disabled} /></td>
      <td>${c.name}${c.locked ? " <span class='badge'>required</span>" : ""}
          <div class="cat-note">${c.notes}</div></td>
      <td>${c.tables.length}</td>
      <td class="rows">${c.rows.toLocaleString()}</td>
      <td class="size">${humanBytes(c.bytes)}</td>`;
    tbody.appendChild(tr);
  });
  tbody.querySelectorAll("input.cat").forEach((cb) =>
    cb.addEventListener("change", () => { recalcTotal(); syncTeleOpts(); })
  );
  recalcTotal();
  syncTeleOpts();
}

async function connect() {
  if (!invoke) { setStatus("Not running inside Tauri.", true); return; }
  $("connect").disabled = true;
  setStatus("Connecting…");
  try {
    const res = await invoke("inspect", conn());
    categories = res.categories;
    connected = true;
    setConnStatus(`Connected — ${res.server.database} (${res.table_count} tables, ${humanBytes(res.total_bytes)})`, "ok");
    setStatus(
      `Connected to '${res.server.database}' — ${res.server.postgres_version.split(" on ")[0]}` +
      (res.server.timescaledb_version ? `, TimescaleDB ${res.server.timescaledb_version}` : "") +
      ` — ${res.table_count} tables, ${humanBytes(res.total_bytes)} total.`
    );
    const c = conn();
    try {
      await invoke("setting_set", {
        key: "last_conn",
        value: JSON.stringify({ host: c.host, port: c.port, dbname: c.dbname, user: c.user }),
      });
    } catch { /* store unavailable */ }
    renderCategories();
    updateGating();
    showView("backup");
  } catch (e) {
    connected = false;
    setStatus("Error: " + e, true);
    setConnStatus("Connection failed", "err");
    updateGating();
  } finally {
    $("connect").disabled = false;
  }
}

function setStatus(msg, isErr) {
  const s = $("status");
  s.textContent = msg;
  s.classList.toggle("err", !!isErr);
}

// ---- Navigation --------------------------------------------------------------
let connected = false;

function showView(name) {
  document.querySelectorAll(".sidebar .nav").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === name)
  );
  document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
  const page = $(name + "Page");
  if (page) page.classList.add("active");
  if (name === "history") loadHistory();
}

function setConnStatus(text, state) {
  const el = $("connStatus");
  el.className = "conn-status" + (state ? " " + state : "");
  el.innerHTML = `<span class="dot"></span> ${text}`;
}

function updateGating() {
  $("backupNeedsConn").style.display = connected ? "none" : "";
  $("backupBody").style.display = connected ? "" : "none";
  $("restoreNeedsConn").style.display = connected ? "none" : "";
  $("restoreBody").style.display = connected ? "" : "none";
}

// ---- Saved connection profiles ----------------------------------------------
async function loadProfiles() {
  try { profilesList = await invoke("list_connections"); } catch { profilesList = []; }
  const sel = $("profiles");
  const cur = sel.value;
  sel.innerHTML = '<option value="">— saved connections —</option>' +
    profilesList.map((p) => `<option value="${p.id}">${p.name}</option>`).join("");
  sel.value = cur;
}

async function onProfileSelect() {
  const id = Number($("profiles").value);
  if (!id) return;
  const p = profilesList.find((x) => x.id === id);
  if (!p) return;
  $("host").value = p.host; $("port").value = p.port;
  $("dbname").value = p.dbname; $("user").value = p.username;
  $("password").value = "";
  if (p.has_password) {
    try {
      const pw = await invoke("get_connection_password", { id });
      if (pw != null) $("password").value = pw;
    } catch { /* keychain unavailable */ }
  }
}

async function saveProfile() {
  const c = conn();
  const name = `${c.user}@${c.host}:${c.port}/${c.dbname}`;
  const existing = profilesList.find((p) => p.name === name);
  try {
    const res = await invoke("save_connection", {
      profile: {
        id: existing ? existing.id : null,
        name, host: c.host, port: c.port, dbname: c.dbname,
        username: c.user, password: c.password || null,
      },
    });
    await loadProfiles();
    $("profiles").value = String(res.id);
    if (c.password && !res.password_saved) {
      setStatus(`Saved '${name}' — but the password could not be stored: no system keychain is available. ` +
        `On Linux, run a Secret Service (install gnome-keyring, or enable KWallet's Secret Service).`, true);
    } else {
      setStatus(`Saved '${name}'` + (res.password_saved ? " (password in the OS keychain)." : "."));
    }
  } catch (e) { setStatus("Save failed: " + e, true); }
}

async function deleteProfile() {
  const id = Number($("profiles").value);
  if (!id) return;
  try { await invoke("delete_connection", { id }); await loadProfiles(); $("profiles").value = ""; }
  catch (e) { setStatus("Delete failed: " + e, true); }
}

// ---- History -----------------------------------------------------------------
function fmtWhen(ts) { try { return new Date(ts).toLocaleString(); } catch { return ts; } }
function statusBadge(s) {
  const color = s === "success" ? "var(--accent)" : "var(--err)";
  return `<span style="color:${color}; font-weight:600">${s || ""}</span>`;
}
function baseName(p) { return (p || "").split(/[\\/]/).pop(); }

async function loadHistory() {
  try {
    const b = await invoke("list_backup_history");
    $("bHist").innerHTML = b.map((r) => `<tr>
      <td>${fmtWhen(r.ts)}</td><td>${r.dbname || ""}</td><td>${r.telemetry || ""}</td>
      <td class="size">${r.status === "success" ? humanBytes(r.archive_bytes) : ""}</td>
      <td>${statusBadge(r.status)}</td>
      <td class="cat-note" title="${r.message || r.output || ""}">${baseName(r.output) || (r.message || "")}</td></tr>`).join("")
      || `<tr><td colspan="6" class="cat-note">No backups yet.</td></tr>`;
    const rr = await invoke("list_restore_history");
    $("rHist").innerHTML = rr.map((r) => `<tr>
      <td>${fmtWhen(r.ts)}</td><td>${r.dbname || ""}</td><td>${statusBadge(r.status)}</td>
      <td class="cat-note" title="${r.message || r.input || ""}">${baseName(r.input)}</td></tr>`).join("")
      || `<tr><td colspan="4" class="cat-note">No restores yet.</td></tr>`;
  } catch (e) { setStatus("History error: " + e, true); }
}

async function clearHistory() {
  try { await invoke("clear_history"); await loadHistory(); } catch (e) { setStatus("Clear failed: " + e, true); }
}

// ---- Backup ------------------------------------------------------------------
function skipList() {
  const skip = [];
  document.querySelectorAll("input.cat[type=checkbox]").forEach((cb) => {
    if (!cb.checked && !cb.disabled) skip.push(cb.dataset.cid);
  });
  return skip;
}

async function backup() {
  const skip = skipList();
  const teleIncluded = !skip.includes("telemetry_historical");
  const mode = document.querySelector('input[name="teleMode"]:checked')?.value;
  const telemetryDays = teleIncluded && mode === "days" ? Number($("teleDays").value) : null;

  let passphrase = null;
  if ($("encryptChk").checked) {
    const p1 = $("encPass").value, p2 = $("encPass2").value;
    if (!p1) { $("encHint").textContent = "Enter a password."; return; }
    if (p1 !== p2) { $("encHint").textContent = "Passwords don't match."; return; }
    passphrase = p1;
  }
  $("encHint").textContent = "";

  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "");
  const defaultName = `${$("dbname").value}-${stamp}.sentient-backup`;
  const output = await invoke("pick_save_path", { defaultName });
  if (!output) return; // cancelled

  $("backupBtn").disabled = true;
  $("backupStatus").textContent = "";
  backupProgress.start();
  try {
    const res = await invoke("backup", {
      ...conn(),
      output,
      skip,
      telemetryDays,
      fileStores: [],
      passphrase,
      onProgress: backupProgress.channel(),
    });
    backupProgress.succeed();
    $("backupStatus").textContent = `Done — ${humanBytes(res.archive_bytes)} → ${res.output}`;
  } catch (e) {
    backupProgress.fail();
    $("backupStatus").textContent = "Backup failed: " + e;
  } finally {
    $("backupBtn").disabled = false;
  }
}

// ---- Restore -----------------------------------------------------------------
async function createDb() {
  const name = $("newDbName").value.trim();
  if (!name) { $("createDbStatus").textContent = "Enter a name."; return; }
  $("createDbBtn").disabled = true;
  $("createDbStatus").textContent = "Creating…";
  try {
    await invoke("create_database", { ...conn(), name });
    $("dbname").value = name;
    await connect();          // connect + inspect the (empty) new DB
    showView("restore");      // stay on the restore tab
    $("createDbStatus").textContent = `Created '${name}' — now choose a file and Restore.`;
  } catch (e) {
    $("createDbStatus").textContent = "Failed: " + e;
  } finally {
    $("createDbBtn").disabled = false;
  }
}

async function pickRestoreFile() {
  const p = await invoke("pick_open_path");
  if (!p) return;
  restoreFile = p;
  $("pickedName").textContent = p.split(/[\\/]/).pop();
  $("restoreBtn").disabled = false;
  try {
    const enc = await invoke("is_encrypted", { path: p });
    $("restorePassRow").style.display = enc ? "" : "none";
    if (!enc) $("restorePass").value = "";
  } catch { $("restorePassRow").style.display = "none"; }
}

async function restore() {
  if (!restoreFile) return;
  $("restoreBtn").disabled = true;
  $("pickBtn").disabled = true;
  $("restoreStatus").textContent = "";
  restoreProgress.start();
  try {
    const passphrase = $("restorePassRow").style.display !== "none" ? $("restorePass").value : null;
    const res = await invoke("restore", {
      ...conn(),
      input: restoreFile,
      allowNonempty: false,
      fileStorePaths: [],
      passphrase,
      onProgress: restoreProgress.channel(),
    });
    restoreProgress.succeed();
    $("restoreStatus").textContent = `Restored into '${res.database}'.`;
  } catch (e) {
    restoreProgress.fail();
    $("restoreStatus").textContent = "Restore failed: " + e;
  } finally {
    $("restoreBtn").disabled = false;
    $("pickBtn").disabled = false;
  }
}

// ---- Init + wiring -----------------------------------------------------------
function toggleEncrypt() {
  const on = $("encryptChk").checked;
  $("encryptFields").style.display = on ? "" : "none";
  try { invoke("setting_set", { key: "encrypt_default", value: on ? "1" : "0" }); } catch { /* store off */ }
}

async function init() {
  initTheme();
  updateGating();
  if (!invoke) return;
  await loadProfiles();
  try {
    const last = await invoke("setting_get", { key: "last_conn" });
    if (last) {
      const c = JSON.parse(last);
      if (c.host) $("host").value = c.host;
      if (c.port) $("port").value = c.port;
      if (c.dbname) $("dbname").value = c.dbname;
      if (c.user) $("user").value = c.user;
    }
    if ((await invoke("setting_get", { key: "encrypt_default" })) === "1") {
      $("encryptChk").checked = true;
      $("encryptFields").style.display = "";
    }
  } catch { /* no saved settings */ }
}

$("themeToggle").addEventListener("click", toggleTheme);
$("connect").addEventListener("click", connect);
$("backupBtn").addEventListener("click", backup);
$("createDbBtn").addEventListener("click", createDb);
$("pickBtn").addEventListener("click", pickRestoreFile);
$("restoreBtn").addEventListener("click", restore);
$("profiles").addEventListener("change", onProfileSelect);
$("saveProfileBtn").addEventListener("click", saveProfile);
$("deleteProfileBtn").addEventListener("click", deleteProfile);
$("clearHistoryBtn").addEventListener("click", clearHistory);
$("encryptChk").addEventListener("change", toggleEncrypt);
document.querySelectorAll(".sidebar .nav").forEach((b) =>
  b.addEventListener("click", () => showView(b.dataset.view))
);
document.querySelectorAll('input[name="themeMode"]').forEach((r) =>
  r.addEventListener("change", () => applyThemeMode(r.value))
);
init();
