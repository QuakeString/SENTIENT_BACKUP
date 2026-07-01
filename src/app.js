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

function renderCategories() {
  const tbody = $("cats");
  tbody.innerHTML = "";
  categories.forEach((c, i) => {
    const tr = document.createElement("tr");
    const checked = c.default_selected ? "checked" : "";
    const disabled = c.locked ? "disabled" : "";
    tr.innerHTML = `
      <td><input class="cat" type="checkbox" data-i="${i}" ${checked} ${disabled} /></td>
      <td>${c.name}${c.locked ? " <span class='badge'>required</span>" : ""}
          <div class="cat-note">${c.notes}</div></td>
      <td>${c.tables.length}</td>
      <td class="rows">${c.rows.toLocaleString()}</td>
      <td class="size">${humanBytes(c.bytes)}</td>`;
    tbody.appendChild(tr);
  });
  tbody.querySelectorAll("input.cat").forEach((cb) =>
    cb.addEventListener("change", recalcTotal)
  );
  recalcTotal();
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
