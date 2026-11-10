"use strict";

const dialog = document.querySelector("#settings");
const form = document.querySelector("#settings-form");
const error = document.querySelector("#settings-error");
const rows = document.querySelector("#downloads");
const empty = document.querySelector("#empty");
const status = document.querySelector("#service-status");
const loginPanel = document.querySelector("#login-panel");
const loginForm = document.querySelector("#login-form");
const loginMessage = document.querySelector("#login-message");
const passwordDialog = document.querySelector("#password-dialog");
const passwordForm = document.querySelector("#password-form");
const phases = {
  setup: "等待设置", checking: "检查网盘目录", downloading: "正在下载",
  idle: "等待下一次检查", login_failed: "百度网盘登录失败，请检查 Token",
  download_failed: "本轮有下载错误",
};
const labels = {
  queued: "排队中", active: "下载中", retrying: "正在重试",
  complete: "已完成", existing: "已存在", failed: "失败",
};
let current;
let setupShown = false;
let session;
let sessionGeneration = 0;

function showSession(next, message) {
  session = next;
  sessionGeneration++;
  const signedIn = Boolean(session.username);
  loginPanel.hidden = signedIn;
  loginForm.hidden = !session.account_configured;
  document.querySelector("#downloads-panel").hidden = !signedIn;
  document.querySelector("#account-actions").hidden = !signedIn;
  document.querySelector("#signed-in-user").textContent = session.username || "";
  loginMessage.textContent = message || (session.account_configured
    ? "使用预先配置的账号登录。"
    : "请在服务器上初始化允许登录的账号。");
  if (!signedIn) {
    passwordDialog.close();
    dialog.close();
    current = undefined;
    setupShown = false;
    rows.replaceChildren();
  }
}

async function refreshSession(message) {
  const generation = sessionGeneration;
  const response = await postJson("/api/v1/auth/session", {});
  if (!response.ok) throw new Error("无法连接登录服务。");
  const next = await response.json();
  if (generation === sessionGeneration) showSession(next, message);
}

function postJson(path, payload) {
  return fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}

async function submitJson(path, payload) {
  const response = await postJson(path, payload);
  if (!response.ok) {
    const details = await response.json().catch(() => ({}));
    throw new Error(details.error || "请求失败，请稍后再试。");
  }
}

function bindForm(form, onSubmit) {
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const submit = form.querySelector('[type="submit"]');
    const message = form.querySelector(".error");
    submit.disabled = true;
    message.textContent = "";
    try {
      await onSubmit(Object.fromEntries(new FormData(form)));
    } catch (failure) {
      message.textContent = failure.message;
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

document.querySelector("#logout").addEventListener("click", async () => {
  try {
    await submitJson("/api/v1/auth/logout", {});
    await refreshSession();
  } catch (failure) {
    status.textContent = failure.message;
  }
});

function openSettings() {
  if (dialog.open) return;
  form.reset();
  error.textContent = "";
  form.elements.monitor_directory.value = current?.monitor_directory || "";
  form.elements.download_directory.value = current?.download_directory || "/downloads";
  const saved = current?.has_tokens === true;
  form.elements.bduss.required = !saved;
  form.elements.stoken.required = !saved;
  document.querySelector("#settings-description").textContent = saved
    ? "留空会保留已保存的 Token。"
    : "填入百度网盘的 BDUSS 和 STOKEN，然后设置监控目录和保存位置。";
  dialog.showModal();
}

document.querySelector("#open-settings").addEventListener("click", openSettings);
document.querySelector("#close-settings").addEventListener("click", () => dialog.close());

bindForm(form, async (payload) => {
  await submitJson("/api/v1/baidu/save-settings", payload);
  dialog.close();
  status.textContent = "设置已保存";
});

dialog.addEventListener("close", () => form.reset());

document.querySelector("#open-password").addEventListener("click", () => {
  passwordForm.reset();
  passwordForm.querySelector(".error").textContent = "";
  passwordDialog.showModal();
});
document.querySelector("#close-password").addEventListener("click", () => passwordDialog.close());
passwordDialog.addEventListener("close", () => passwordForm.reset());
bindForm(passwordForm, async ({ current_password, new_password, confirm_password }) => {
  if (new_password !== confirm_password) throw new Error("两次输入的新密码不一致。");
  await submitJson("/api/v1/auth/change-password", { current_password, new_password });
  passwordDialog.close();
  await refreshSession("密码已更新，请重新登录。");
});

function formatBytes(value) {
  if (!Number.isFinite(value)) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return new Intl.NumberFormat("zh-CN", { maximumFractionDigits: unit ? 1 : 0 })
    .format(value) + " " + units[unit];
}

function renderFile(file, provider) {
  const row = document.createElement("tr");
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
    bar.setAttribute("aria-label", name.textContent + " 的下载进度");
    if (percent !== null) bar.value = percent;
    const value = document.createElement("span");
    value.textContent = percent === null ? "准备中" : percent.toFixed(1) + "%";
    progress.append(bar, value);
    if (active && file.total > 0) {
      const size = document.createElement("small");
      size.textContent = formatBytes(file.downloaded) + " / " + formatBytes(file.total);
      progress.append(size);
    }
  } else {
    progress.textContent = "—";
  }

  row.insertCell().textContent = active ? formatBytes(file.speed) + "/s" : "—";
  const badge = row.insertCell();
  badge.textContent = labels[file.status] || file.status;
  row.insertCell().textContent = provider === "baidu" ? "百度网盘" : provider;
  if (active) row.className = "active";
  return row;
}

async function refresh() {
  try {
    if (document.hidden || dialog.open || passwordDialog.open) return;
    if (!session || !session.account_configured) {
      await refreshSession();
    }
    if (!session?.username) return;
    const generation = sessionGeneration;
    const response = await postJson("/api/v1/baidu/status", {});
    if (generation !== sessionGeneration) return;
    if (response.status === 401) {
      await refreshSession("登录已过期，请重新登录。");
      return;
    }
    if (!response.ok) throw new Error();
    const next = await response.json();
    if (generation !== sessionGeneration || dialog.open || passwordDialog.open) return;
    current = next;
    status.textContent = phases[current.phase] || "正在启动";
    document.querySelector("#monitor-directory").textContent = current.monitor_directory || "尚未选择监控目录";
    const nextRows = document.createDocumentFragment();
    for (const file of current.files) nextRows.append(renderFile(file, current.provider));
    rows.replaceChildren(nextRows);
    empty.hidden = current.files.length > 0;
    empty.textContent = current.setup_required ? "保存设置后开始检查文件。" : "当前没有下载任务。";
    if (current.setup_required && !setupShown) {
      setupShown = true;
      openSettings();
    }
  } catch {
    (session?.username ? status : loginMessage).textContent = "连接中断，正在重试";
  } finally {
    setTimeout(refresh, 1000);
  }
}

refresh();
