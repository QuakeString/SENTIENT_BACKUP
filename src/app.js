// Phase-0 frontend. Uses Tauri's global `invoke` (withGlobalTauri = true).
// Later phases will move this to Svelte/TS with a proper build step.

const invoke = window.__TAURI__?.core?.invoke;

const $ = (id) => document.getElementById(id);
let categories = []; // last inspect result

function humanBytes(b) {
  const u = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${b} B` : `${v.toFixed(1)} ${u[i]}`;
}

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
  // Telemetry range controls are only relevant when the historical-telemetry
  // component is selected.
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

// Ids of unchecked, skippable components (locked ones are disabled+checked).
function skipList() {
  const skip = [];
  document.querySelectorAll("input.cat[type=checkbox]").forEach((cb) => {
    if (!cb.checked && !cb.disabled) skip.push(cb.dataset.cid);
  });
  return skip;
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

function onProgress(p) {
  if (p.type === "step") {
    $("progressStep").textContent = `[${p.index}/${p.total}] ${p.name}`;
  } else if (p.type === "log") {
    const el = $("progressLog");
    el.textContent += p.line + "\n";
    el.scrollTop = el.scrollHeight;
  } else if (p.type === "done") {
    $("progressStep").textContent = "✓ " + p.message;
  }
}

async function backup() {
  const skip = skipList();
  const teleIncluded = !skip.includes("telemetry_historical");
  const mode = document.querySelector('input[name="teleMode"]:checked')?.value;
  const telemetryDays =
    teleIncluded && mode === "days" ? Number($("teleDays").value) : null;

  const stamp = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "");
  const defaultName = `${$("dbname").value}-${stamp}.sentient-backup`;
  const output = await invoke("pick_save_path", { defaultName });
  if (!output) return; // cancelled

  $("backupBtn").disabled = true;
  $("backupStatus").textContent = "";
  $("progressLog").textContent = "";
  $("progressStep").textContent = "Starting…";
  $("progressPanel").style.display = "";

  const channel = new window.__TAURI__.core.Channel();
  channel.onmessage = onProgress;

  try {
    const res = await invoke("backup", {
      ...conn(),
      output,
      skip,
      telemetryDays,
      fileStores: [],
      onProgress: channel,
    });
    $("backupStatus").textContent =
      `Backup complete — ${humanBytes(res.archive_bytes)} → ${res.output}`;
  } catch (e) {
    $("progressStep").textContent = "";
    $("backupStatus").textContent = "Backup failed: " + e;
  } finally {
    $("backupBtn").disabled = false;
  }
}

async function connect() {
  if (!invoke) { $("status").textContent = "Not running inside Tauri."; return; }
  $("connect").disabled = true;
  $("status").textContent = "Connecting…";
  try {
    const res = await invoke("inspect", {
      host: $("host").value,
      port: Number($("port").value),
      dbname: $("dbname").value,
      user: $("user").value,
      password: $("password").value,
    });
    categories = res.categories;
    $("status").textContent =
      `Connected to '${res.server.database}' — ${res.server.postgres_version.split(" on ")[0]}` +
      (res.server.timescaledb_version ? `, TimescaleDB ${res.server.timescaledb_version}` : "") +
      ` — ${res.table_count} tables, ${humanBytes(res.total_bytes)} total.`;
    $("backupPanel").style.display = "";
    renderCategories();
  } catch (e) {
    $("status").textContent = "Error: " + e;
    $("backupPanel").style.display = "none";
  } finally {
    $("connect").disabled = false;
  }
}

$("connect").addEventListener("click", connect);
$("backupBtn").addEventListener("click", backup);
