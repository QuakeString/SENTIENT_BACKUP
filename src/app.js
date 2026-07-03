// Frontend for the SENTIENT Backup & Restore desktop app. Uses Tauri's global
// `invoke` / `Channel` (withGlobalTauri = true) — no bundler.

const invoke = window.__TAURI__?.core?.invoke;
const Channel = window.__TAURI__?.core?.Channel;

const $ = (id) => document.getElementById(id);
let categories = []; // last inspect result
let restoreFile = null; // chosen archive path

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
    setStatus(
      `Connected to '${res.server.database}' — ${res.server.postgres_version.split(" on ")[0]}` +
      (res.server.timescaledb_version ? `, TimescaleDB ${res.server.timescaledb_version}` : "") +
      ` — ${res.table_count} tables, ${humanBytes(res.total_bytes)} total.`
    );
    $("tabs").style.display = "";
    renderCategories();
    showView("backup");
  } catch (e) {
    setStatus("Error: " + e, true);
    $("tabs").style.display = "none";
    document.querySelectorAll(".view").forEach((v) => (v.style.display = "none"));
  } finally {
    $("connect").disabled = false;
  }
}

function setStatus(msg, isErr) {
  const s = $("status");
  s.textContent = msg;
  s.classList.toggle("err", !!isErr);
}

// ---- Tabs --------------------------------------------------------------------
function showView(name) {
  document.querySelectorAll(".tabs button").forEach((b) =>
    b.classList.toggle("active", b.dataset.view === name)
  );
  $("backupView").style.display = name === "backup" ? "" : "none";
  $("restoreView").style.display = name === "restore" ? "" : "none";
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
async function pickRestoreFile() {
  const p = await invoke("pick_open_path");
  if (!p) return;
  restoreFile = p;
  $("pickedName").textContent = p.split(/[\\/]/).pop();
  $("restoreBtn").disabled = false;
}

async function restore() {
  if (!restoreFile) return;
  $("restoreBtn").disabled = true;
  $("pickBtn").disabled = true;
  $("restoreStatus").textContent = "";
  restoreProgress.start();
  try {
    const res = await invoke("restore", {
      ...conn(),
      input: restoreFile,
      allowNonempty: false,
      fileStorePaths: [],
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

// ---- Wiring ------------------------------------------------------------------
$("connect").addEventListener("click", connect);
$("backupBtn").addEventListener("click", backup);
$("pickBtn").addEventListener("click", pickRestoreFile);
$("restoreBtn").addEventListener("click", restore);
document.querySelectorAll(".tabs button").forEach((b) =>
  b.addEventListener("click", () => showView(b.dataset.view))
);
