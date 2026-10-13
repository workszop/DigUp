// DigUp's setup window: what to search and the shortcut, with the model downloading in the background from the moment
// it opens (the shell starts that), as the Mac app's onboarding does. Folders show what's in them (`digup estimate`):
// counts, not times, until the estimates are measured on more than the Mac. Under them, code search's folders (the Mac
// app's Settings → Code): none checked turns code search off.
"use strict";

const tauri = window.__TAURI__;
const invoke = (command, args) => tauri.core.invoke(command, args);
const isMac = /Mac/.test(navigator.platform);
const isWindows = /Win/.test(navigator.platform);
const PAGES = ["welcome", "folders", "shortcut", "ready"];

const $ = (id) => document.getElementById(id);
const folders = new Map();       // path → {checked, row, counts}
const codeFolders = new Map();   // the same, for code search
let page = 0;
let info = { offered: [], chosen: [], codeChosen: [], hotkey: "", model: { state: "checking" }, auto: false };

async function start() {
  info = await invoke("setup_info");
  // The usual folders, then any chosen before (Choose Folders… after setup). On a first launch they start checked,
  // all but Downloads (much of what lands there is passing through).
  const chosen = new Set(info.chosen);
  // Code folders alone count as set up: Choose Folders… then opens on the folders, the usual ones unchecked.
  const firstTime = chosen.size === 0 && !(info.codeChosen || []).length;
  for (const path of info.offered) addFolder(path, firstTime ? !/[\\/]Downloads$/i.test(path) || info.auto : chosen.has(path));
  for (const path of info.chosen) if (!folders.has(path)) addFolder(path, true);
  for (const path of info.codeChosen || []) addFolder(path, true, codeFolders);
  estimate([...folders.keys()]);
  estimate([...codeFolders.keys()], codeFolders);
  showModel(info.model);
  tauri.event.listen("model", ({ payload }) => showModel(payload));
  showKeys(info.hotkey);
  if (!firstTime) page = 1;   // Choose Folders…: straight to the folders
  const asked = PAGES.indexOf(new URLSearchParams(location.search).get("page"));   // dev snapshots: ?page=folders
  if (asked >= 0) page = asked;
  render();
  if (info.auto) setTimeout(clickThrough, 500);
}

// MARK: Pages

function render() {
  PAGES.forEach((name, index) => { $(name).hidden = index !== page; });
  $("back").hidden = page === 0;
  $("next").textContent = page === PAGES.length - 1 ? "Start" : "Continue";
  // Code folders alone are enough: DigUp then searches only code.
  $("next").disabled = page === 1 && !chosenFolders().length && !chosenFolders(codeFolders).length;
}

$("next").addEventListener("click", async () => {
  if (page < PAGES.length - 1) {
    page += 1;
    render();
    return;
  }
  $("next").disabled = true;
  try {
    await invoke("finish_setup", { folders: chosenFolders(), codeFolders: chosenFolders(codeFolders) });
  } catch (error) {
    $("model").textContent = String(error);
    $("next").disabled = false;
  }
});

$("back").addEventListener("click", () => {
  page = Math.max(0, page - 1);
  render();
});

/// Debug runs (--debug-setup): every offered folder, every page, then Start.
async function clickThrough() {
  while (page < PAGES.length - 1) {
    $("next").click();
    await new Promise((resolve) => setTimeout(resolve, 300));
  }
  $("next").click();
}

// MARK: Folders

/// A row in the folders' list, or with `list` codeFolders, in the code folders'.
function addFolder(path, checked, list = folders) {
  const row = document.createElement("label");
  row.className = "folder";
  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = checked;
  box.addEventListener("change", () => {
    list.get(path).checked = box.checked;
    render();
  });
  const text = document.createElement("div");
  text.className = "text";
  const name = document.createElement("div");
  name.className = "name";
  name.textContent = path.split(/[\\/]/).filter(Boolean).pop() || path;
  const where = document.createElement("div");
  where.className = "path";
  where.textContent = isWindows ? path.replaceAll("/", "\\") : path;
  text.append(name, where);
  const counts = document.createElement("div");
  counts.className = "counts";
  counts.textContent = "Counting…";
  if (list === codeFolders) counts.title = CODE_HELP;
  row.append(box, text, counts);
  $(list === codeFolders ? "code-list" : "folder-list").append(row);
  list.set(path, { checked, row, counts });
}

function chosenFolders(list = folders) {
  return [...list].filter(([, folder]) => folder.checked).map(([path]) => path);
}

/// What's in each folder, from the helper; for code folders, what code search reads there (`digup estimate --code`).
async function estimate(paths, list = folders) {
  if (!paths.length) return;
  const code = list === codeFolders;
  let answers = [];
  try {
    answers = await invoke("estimate", { folders: paths, code });
  } catch (error) {
    for (const path of paths) list.get(path).counts.textContent = "";
    return;
  }
  for (const path of paths) {
    const answer = answers.find((item) => same(item.folder, path));
    list.get(path).counts.textContent = !answer ? "" : code ? codeText(answer) : countsText(answer);
  }
}

/// The helper reports folders with forward slashes (and Windows paths compare without case).
function same(a, b) {
  const plain = (path) => (path || "").replaceAll("\\", "/").replace(/\/$/, "");
  return isWindows ? plain(a).toLowerCase() === plain(b).toLowerCase() : plain(a) === plain(b);
}

/// "1,204 pictures · 37 PDFs · 12 documents · 3 videos"
function countsText(answer) {
  if (answer.readable === false) return "Can't read it";
  const files = answer.files || {};
  const part = (count, one, many) => (count ? `${count.toLocaleString()} ${count === 1 ? one : many}` : null);
  const parts = [
    part((files.image || 0) + (files.screenshot || 0), "picture", "pictures"),
    part(files.pdf, "PDF", "PDFs"),
    part(files.doc, "document", "documents"),
    part(files.audio, "recording", "recordings"),
    part(files.video, "video", "videos"),
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "Nothing DigUp searches in it";
}

const CODE_HELP = "About how much room the code index needs for it. DigUp reads the code once; after that, only what "
  + "changes is read again.";

/// "39 code files · about 370 KB of index"
function codeText(answer) {
  if (answer.readable === false) return "Can't read it";
  const count = (answer.files || {}).code || 0;
  if (!count) return "No code here";
  const files = `${count.toLocaleString()} code ${count === 1 ? "file" : "files"}`;
  return answer.bytes > 0 ? `${files} · about ${roughBytes(answer.bytes)} of index` : files;
}

/// Sizes to two figures, the way file managers count them (1 KB = 1,000 bytes): "370 KB", "1.2 GB".
function roughBytes(count) {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let value = count, unit = 0;
  while (value >= 1000 && unit < units.length - 1) { value /= 1000; unit += 1; }
  const shown = value >= 100 ? Math.round(value / 10) * 10 : value >= 10 ? Math.round(value) : Number(value.toFixed(1));
  return `${shown.toLocaleString()} ${units[unit]}`;
}

/// Folders from the system's picker into a list, checked, and their estimates asked for.
async function addPicked(list) {
  const picked = (await invoke("pick_folders", { code: list === codeFolders })).filter((path) => !list.has(path));
  for (const path of picked) addFolder(path, true, list);
  estimate(picked, list);
  render();
}

$("add-folder").addEventListener("click", () => addPicked(folders));
$("add-code-folder").addEventListener("click", () => addPicked(codeFolders));

// MARK: The shortcut and the model

function showKeys(hotkey) {
  const keys = isMac ? [...hotkey.replace(/[^⌃⌥⇧⌘]/g, "")].concat(hotkey.replace(/[⌃⌥⇧⌘]/g, "")) : hotkey.split("+");
  $("keys").replaceChildren(...keys.filter(Boolean).map((key) => {
    const cap = document.createElement("span");
    cap.className = "key";
    cap.textContent = key;
    return cap;
  }));
  $("reveal-key").textContent = isMac ? "⌘↩" : "Ctrl+↩";
  $("reveal-text").textContent = isMac ? "shows it in Finder" : isWindows ? "shows it in Explorer" : "shows it in its folder";
}

function showModel(model) {
  const megabytes = (bytes) => Math.round(bytes / 1e6).toLocaleString();
  const lines = {
    checking: "Checking the model…",
    waiting: "Waiting for the network to download the model",
    ready: "Model ready",
    failed: `The model couldn't download: ${model.message || "unknown error"}`,
  };
  let text = lines[model.state] || "";
  if (model.state === "downloading") {
    text = model.total
      ? `Downloading the model · ${Math.floor((100 * model.bytes) / model.total)}% of ${megabytes(model.total)} MB`
      : "Downloading the model…";
    if (model.problem) text += ` (${model.problem}, trying again)`;
  }
  $("model").textContent = text;
}

start();
