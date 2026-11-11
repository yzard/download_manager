import { translate } from "./locale.js";

const baseUrl = new URL(".", import.meta.url);

const dialog = document.querySelector("#settings");
const form = document.querySelector("#settings-form");
const generalForm = document.querySelector("#general-settings-form");
const settingsDescription = document.querySelector("#settings-description");
const settingsButton = document.querySelector("#open-settings");
const rows = document.querySelector("#downloads");
const empty = document.querySelector("#empty");
const status = document.querySelector("#service-status");
const loginPanel = document.querySelector("#login-panel");
const loginForm = document.querySelector("#login-form");
const loginTitle = document.querySelector("#login-title");
const accountSetupForm = document.querySelector("#account-setup-form");
const loginMessage = document.querySelector("#login-message");
const passwordDialog = document.querySelector("#password-dialog");
const passwordForm = document.querySelector("#password-form");
let language = document.documentElement.lang;
let general;
let current;
let setupShown = false;
let session;
let sessionGeneration = 0;
let renewAt = Infinity;
let renewal;

function text(key) {
  return translate(language, key);
}

function showMessage(element, key) {
  element.dataset.message = key;
  element.textContent = text(key) || "";
}

function applyLanguage(next) {
  if (next === language) return;
  language = next;
  document.documentElement.lang = language;
  for (const element of document.querySelectorAll("[data-text]")) {
    element.textContent = text(element.dataset.text);
  }
  for (const element of document.querySelectorAll("[data-label]")) {
    const label = text(element.dataset.label);
    element.setAttribute("aria-label", label);
    element.title = label;
  }
  for (const handle of document.querySelectorAll(".column-resize")) {
    labelResizeHandle(handle);
  }
  for (const element of document.querySelectorAll("[data-message]")) {
    showMessage(element, element.dataset.message);
  }
  if (current) renderDownloads();
}

function showSession(next, message) {
  session = next;
  renewAt = next.refresh_in === null ? Infinity : performance.now() + next.refresh_in * 1000;
  general = next.general;
  applyLanguage(next.language);
  sessionGeneration++;
  const signedIn = Boolean(session.username);
  loginPanel.hidden = signedIn;
  accountSetupForm.hidden = !session.account_setup;
  if (!session.account_setup) accountSetupForm.reset();
  loginForm.hidden = Boolean(session.account_setup);
  showMessage(loginTitle, {create: "createAccount", reset: "resetAccount"}[session.account_setup] || "login");
  document.querySelector("#downloads-panel").hidden = !signedIn;
  document.querySelector("#account-actions").hidden = !signedIn;
  document.querySelector("#signed-in-user").textContent = session.username || "";
  showMessage(loginMessage, message || (session.account_setup ? "passwordMinimum" : "loginPrompt"));
  if (!signedIn) {
    passwordDialog.close();
    dialog.close();
    current = undefined;
    settingsButton.disabled = true;
    setupShown = false;
    rows.replaceChildren();
  }
}

async function readSession() {
  const response = await postJson("/api/v1/auth/session", {});
  if (!response.ok) throw new Error("sessionUnavailable");
  return response.json();
}

function renewAccess() {
  renewal ??= postJson("/api/v1/auth/refresh", {}).then((response) => {
    if (!response.ok && response.status !== 401) throw new Error("sessionUnavailable");
    return response.ok;
  }).finally(() => { renewal = undefined; });
  return renewal;
}

async function refreshSession(message, renew = false) {
  const generation = sessionGeneration;
  if (renew) await renewAccess();
  let next = await readSession();
  if (generation !== sessionGeneration) return;
  if (!renew && !next.username && next.can_refresh && await renewAccess()) {
    next = await readSession();
  }
  if (generation === sessionGeneration) showSession(next, message);
}

function postJson(path, payload) {
  return fetch(new URL(path.slice(1), baseUrl), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}

async function submitJson(path, payload) {
  let response;
  try {
    response = await postJson(path, payload);
    if (response.status === 401 && session?.username) {
      await refreshSession("sessionExpired", true);
      if (session?.username) response = await postJson(path, payload);
    }
  } catch {
    throw new Error("requestFailed");
  }
  if (!response.ok) {
    const details = await response.json().catch(() => ({}));
    throw new Error(text(details.error) ? details.error : "requestFailed");
  }
  return response;
}

function bindForm(form, onSubmit) {
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const submit = form.querySelector('[type="submit"]');
    const message = form.querySelector(".error");
    submit.disabled = true;
    showMessage(message, "");
    const saved = form.querySelector('[data-text="settingsSaved"]');
    if (saved) saved.hidden = true;
    try {
      await onSubmit(Object.fromEntries(new FormData(form)));
      if (saved) saved.hidden = false;
    } catch (failure) {
      showMessage(message, text(failure.message) ? failure.message : "requestFailed");
    } finally {
      submit.disabled = false;
    }
  });
}

bindForm(loginForm, async (payload) => {
  await submitJson("/api/v1/auth/login", payload);
  loginForm.reset();
  await refreshSession();
});

bindForm(accountSetupForm, async ({ username, password, confirm_password, setup_code }) => {
  if (password !== confirm_password) throw new Error("passwordMismatch");
  await submitJson("/api/v1/auth/setup-account", { username, password, setup_code });
  loginForm.elements.username.value = username;
  await refreshSession("accountSaved");
  loginForm.elements.password.focus();
});

document.querySelector("#logout").addEventListener("click", async () => {
  try {
    await submitJson("/api/v1/auth/logout", {});
    await refreshSession();
  } catch (failure) {
    showMessage(status, text(failure.message) ? failure.message : "requestFailed");
  }
});

function resetSettingsForms() {
  for (const settingsForm of [generalForm, form]) {
    settingsForm.reset();
    showMessage(settingsForm.querySelector(".error"), "");
    settingsForm.querySelector('[data-text="settingsSaved"]').hidden = true;
  }
}

function updateBaseUrlField() {
  const field = generalForm.elements.base_url;
  field.disabled = session.base_url_from_env;
  field.value = field.disabled ? session.active_base_url : general.base_url;
  const message = field.disabled ? "baseUrlFromEnvironment"
    : general.base_url === session.active_base_url ? "baseUrlHelp" : "baseUrlPending";
  showMessage(document.querySelector("#base-url-help"), message);
}

function openSettings() {
  if (dialog.open || !general || !current) return;
  resetSettingsForms();
  generalForm.elements.download_directory.value = general.download_directory;
  generalForm.elements.language.value = general.language;
  updateBaseUrlField();
  form.elements.monitor_directory.value = current.monitor_directory || "";
  form.elements.check_interval_seconds.value = current.check_interval_seconds;
  form.elements.delete_after_download.checked = current.delete_after_download;
  form.elements.bduss.required = !current.has_tokens;
  form.elements.stoken.required = !current.has_tokens;
  showMessage(settingsDescription, current.has_tokens ? "keepTokens" : "enterTokens");
  dialog.showModal();
}

settingsButton.addEventListener("click", openSettings);
document.querySelector("#close-settings").addEventListener("click", () => dialog.close());

bindForm(generalForm, async (payload) => {
  const response = await submitJson("/api/v1/settings/save-general", payload);
  general = await response.json();
  generalForm.elements.download_directory.value = general.download_directory;
  generalForm.elements.language.value = general.language;
  updateBaseUrlField();
  applyLanguage(general.language);
});

bindForm(form, async (payload) => {
  const check_interval_seconds = Number(payload.check_interval_seconds);
  const delete_after_download = form.elements.delete_after_download.checked;
  await submitJson("/api/v1/baidu/save-settings", {...payload, check_interval_seconds, delete_after_download});
  current.monitor_directory = payload.monitor_directory.trim();
  current.check_interval_seconds = check_interval_seconds;
  current.delete_after_download = delete_after_download;
  current.has_tokens = true;
  current.setup_required = false;
  for (const token of [form.elements.bduss, form.elements.stoken]) {
    token.value = "";
    token.required = false;
  }
  showMessage(settingsDescription, "keepTokens");
});

dialog.addEventListener("close", resetSettingsForms);

document.querySelector("#open-password").addEventListener("click", () => {
  passwordForm.reset();
  showMessage(passwordForm.querySelector(".error"), "");
  passwordDialog.showModal();
});
document.querySelector("#close-password").addEventListener("click", () => passwordDialog.close());
passwordDialog.addEventListener("close", () => passwordForm.reset());
bindForm(passwordForm, async ({ current_password, new_password, confirm_password }) => {
  if (new_password !== confirm_password) throw new Error("两次输入的新密码不一致。");
  await submitJson("/api/v1/auth/change-password", { current_password, new_password });
  passwordDialog.close();
  await refreshSession("passwordChanged");
});

function formatBytes(value) {
  if (!Number.isFinite(value)) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return new Intl.NumberFormat(language, { maximumFractionDigits: unit ? 1 : 0 })
    .format(value) + " " + units[unit];
}

function renderFile(file, provider) {
  const row = document.createElement("tr");
  const source = row.insertCell();
  source.textContent = provider === "baidu" ? text("baidu") : provider;
  source.title = source.textContent;
  const name = row.insertCell();
  name.textContent = file.path.split("/").pop() || file.path;
  name.title = file.path;
  name.className = "filename";

  const progress = row.insertCell();
  const active = file.status === "active" || file.status === "retrying";
  const complete = file.status === "complete" || file.status === "existing";
  if (active || complete) {
    const percent = complete ? 100 : (file.total > 0 ? Math.min(100, 100 * file.downloaded / file.total) : null);
    const bar = document.createElement("progress");
    bar.max = 100;
    bar.setAttribute("aria-label", text("fileProgress").replace("{file}", () => name.textContent));
    if (percent !== null) bar.value = percent;
    const value = document.createElement("span");
    value.textContent = percent === null ? text("preparing") : percent.toFixed(1) + "%";
    progress.append(bar, value);
    if (active && file.total > 0) {
      const size = document.createElement("small");
      size.textContent = formatBytes(file.downloaded) + " / " + formatBytes(file.total);
      progress.append(size);
    }
  } else {
    progress.textContent = "—";
  }

  const speed = row.insertCell();
  speed.textContent = active ? formatBytes(file.speed) + "/s" : "—";
  speed.title = speed.textContent;
  const badge = row.insertCell();
  badge.textContent = text("state_" + file.status) || file.status;
  if (active) row.className = "active";
  return row;
}

function labelResizeHandle(handle) {
  const column = text(handle.parentElement.dataset.column);
  handle.setAttribute("aria-label", text("resizeColumn").replace("{column}", column));
}

function enableColumnResizing() {
  const table = rows.closest("table");
  const defaults = getComputedStyle(table);
  const storageKey = "download-manager-column-widths";
  const minimums = {source: 80, file: 120, progress: 150, speed: 90, status: 90};
  const maximum = 1600;
  let storedWidths;
  try {
    storedWidths = JSON.parse(localStorage.getItem(storageKey));
  } catch {
    // Resizing still works when browser storage is unavailable.
  }

  const widths = {};
  function persistWidths() {
    try {
      localStorage.setItem(storageKey, JSON.stringify(widths));
    } catch {
      // Keep the current widths for this page when persistence is unavailable.
    }
  }

  for (const header of table.querySelectorAll("th[data-column]")) {
    const column = header.dataset.column;
    const minimum = minimums[column];
    const property = "--" + column + "-width";
    const handle = document.createElement("span");
    handle.className = "column-resize";
    handle.tabIndex = 0;
    handle.setAttribute("role", "separator");
    handle.setAttribute("aria-orientation", "vertical");
    handle.setAttribute("aria-valuemin", minimum);
    handle.setAttribute("aria-valuemax", maximum);
    header.append(handle);
    labelResizeHandle(handle);

    function setWidth(requested) {
      const width = Math.max(minimum, Math.min(maximum, Math.round(requested)));
      widths[column] = width;
      table.style.setProperty(property, width + "px");
      handle.setAttribute("aria-valuenow", width);
    }

    const savedWidth = storedWidths?.[column];
    setWidth(Number.isFinite(savedWidth) ? savedWidth : Number.parseInt(defaults.getPropertyValue(property), 10));

    let drag;
    handle.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || drag) return;
      event.preventDefault();
      handle.setPointerCapture(event.pointerId);
      drag = {pointer: event.pointerId, x: event.clientX, width: widths[column]};
    });
    handle.addEventListener("pointermove", (event) => {
      if (drag?.pointer === event.pointerId) setWidth(drag.width + event.clientX - drag.x);
    });
    handle.addEventListener("lostpointercapture", (event) => {
      if (drag?.pointer !== event.pointerId) return;
      drag = undefined;
      persistWidths();
    });
    handle.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      setWidth(widths[column] + (event.key === "ArrowRight" ? 10 : -10));
      persistWidths();
    });
  }
}

function renderDownloads() {
  showMessage(status, text("phase_" + current.phase) ? "phase_" + current.phase : "starting");
  document.querySelector("#monitor-directory").textContent = current.monitor_directory || text("noMonitorDirectory");
  const nextRows = document.createDocumentFragment();
  for (const file of current.files) nextRows.append(renderFile(file, current.provider));
  rows.replaceChildren(nextRows);
  empty.hidden = current.files.length > 0;
  showMessage(empty, current.setup_required ? "saveToStart" : "noDownloads");
}

async function refresh() {
  try {
    if (document.hidden) return;
    if (performance.now() >= renewAt) await refreshSession("sessionExpired", true);
    if (dialog.open || passwordDialog.open) return;
    if (!session || session.account_setup) {
      await refreshSession();
    }
    if (!session?.username) return;
    const generation = sessionGeneration;
    const response = await postJson("/api/v1/baidu/status", {});
    if (generation !== sessionGeneration) return;
    if (response.status === 401) {
      await refreshSession("sessionExpired", true);
      return;
    }
    if (!response.ok) throw new Error();
    const next = await response.json();
    if (generation !== sessionGeneration || dialog.open || passwordDialog.open) return;
    current = next;
    settingsButton.disabled = false;
    renderDownloads();
    if (current.setup_required && !setupShown) {
      setupShown = true;
      openSettings();
    }
  } catch {
    showMessage(session?.username ? status : loginMessage, "reconnecting");
  } finally {
    setTimeout(refresh, 1000);
  }
}

enableColumnResizing();
refresh();
