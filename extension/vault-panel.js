// Vault settings section of the side panel. All file access happens in the native host; this
// only sends requests and renders the host's answers.

import { SearchError } from "./search-client.js";
import { LOCATION_NOTE, VERDICT_TEXT, describeReasons, describeStatus, formatCounts, vaultRequest } from "./vault-client.js";

const $ = (selector) => document.querySelector(selector);

export function createVaultPanel({ send, onVaultChanged }) {
  const panel = $("#vault-panel");
  const toggle = $("#vault-toggle");
  const stateText = $("#vault-state");
  const pathText = $("#vault-path");
  const countsText = $("#vault-counts");
  const locationBox = $("#vault-location");
  const verdictText = $("#vault-verdict");
  const reasonsList = $("#vault-reasons");
  const createDefault = $("#vault-create-default");
  const defaultPathText = $("#vault-default-path");
  const createPick = $("#vault-create-pick");
  const openPick = $("#vault-open-pick");
  const backupPick = $("#vault-backup-pick");
  const manualInput = $("#vault-path-input");
  const approvalBox = $("#vault-approval");
  const approvalTitle = $("#vault-approval-title");
  const approvalReasons = $("#vault-approval-reasons");
  const approveButton = $("#vault-approve");
  const approveCancel = $("#vault-approve-cancel");
  const messageText = $("#vault-message");
  $("#vault-note").textContent = LOCATION_NOTE;

  const actionButtons = [createDefault, createPick, openPick, backupPick, ...document.querySelectorAll("[data-manual]")];
  let status = null;
  let busy = false;
  let pendingApproval = null;

  function setOpen(open) {
    panel.hidden = !open;
    toggle.setAttribute("aria-expanded", String(open));
  }

  function fillList(list, items) {
    list.replaceChildren(...items.map((text) => Object.assign(document.createElement("li"), { textContent: text })));
  }

  function updateButtons() {
    const manageable = status?.managementEnabled === true;
    for (const button of actionButtons) button.disabled = busy || !manageable;
    if (!busy && manageable) {
      createDefault.disabled = status.defaultExists === true;
      backupPick.disabled = status.state !== "ready";
      document.querySelector('[data-manual="vault_backup"]').disabled = status.state !== "ready";
    }
  }

  function render() {
    stateText.textContent = describeStatus(status);
    pathText.hidden = !status?.vaultPath;
    pathText.textContent = status?.vaultPath || "";
    const counts = status?.state === "ready" ? formatCounts(status.counts) : "";
    countsText.hidden = !counts;
    countsText.textContent = counts;

    const location = status?.location;
    locationBox.hidden = !location;
    if (location) {
      locationBox.dataset.verdict = location.verdict;
      const approvedNote = location.verdict === "needs_approval" && status.locationApproved ? "（承認済み）" : "";
      verdictText.textContent = `${VERDICT_TEXT[location.verdict] || location.verdict}${approvedNote}`;
      fillList(reasonsList, describeReasons(location.reasons));
    }

    if (status?.defaultPath) {
      defaultPathText.textContent = status.defaultExists
        ? `既定の場所に既にファイルがあります: ${status.defaultPath}\n開く場合は「既存のVaultを開く」を使ってください。`
        : status.defaultPath;
    } else {
      defaultPathText.textContent = "";
    }
    updateButtons();
  }

  function setMessage(text) {
    messageText.textContent = text;
  }

  async function refresh() {
    try {
      const response = await vaultRequest("vault_status", {}, { send });
      status = response.status || null;
    } catch (error) {
      status = null;
      setMessage(error instanceof SearchError ? error.message : "Vaultの状態を取得できませんでした。");
    }
    render();
    return status;
  }

  function showApproval(type, params, response) {
    pendingApproval = { type, params: { ...params, path: response.path, approveWarnings: true } };
    approvalTitle.textContent = `この場所をActive Vaultにすると、同期による破損やデータ消失の恐れがあります:\n${response.path}`;
    fillList(approvalReasons, describeReasons(response.location?.reasons));
    approvalBox.hidden = false;
    approveButton.focus();
  }

  function successMessage(type, response) {
    if (type === "vault_backup") return `バックアップを作成しました:\n${response.backupPath}`;
    if (type === "vault_create") return `新しいVaultを作成し、使用を開始しました:\n${response.path}`;
    return `Vaultを開きました:\n${response.path}`;
  }

  async function run(type, params = {}) {
    if (busy) return;
    busy = true;
    approvalBox.hidden = true;
    pendingApproval = null;
    updateButtons();
    const usesDialog = !params.path && !params.directory && !params.useDefault;
    setMessage(usesDialog ? "Windowsのダイアログで選択してください。\n表示されない場合はタスクバーを確認してください。" : "処理中…");
    try {
      const response = await vaultRequest(type, params, { send });
      setMessage(successMessage(type, response));
      if (response.status) {
        status = response.status;
        render();
      }
      if (type !== "vault_backup") onVaultChanged();
    } catch (error) {
      const response = error instanceof SearchError ? error.response : null;
      if (error.code === "location_needs_approval" && response?.path) {
        setMessage("");
        showApproval(type, params, response);
      } else {
        const reasons = describeReasons(response?.location?.reasons);
        const lines = [error instanceof SearchError ? error.message : "操作に失敗しました。"];
        if (response?.created) lines.push("Vaultファイルは作成されましたが、使用開始に失敗しました。「既存のVaultを開く」で開けます。");
        if (response?.path) lines.push(response.path);
        if (reasons.length) lines.push(...reasons.map((reason) => `・${reason}`));
        setMessage(lines.join("\n"));
      }
    } finally {
      busy = false;
      await refresh();
    }
  }

  toggle.addEventListener("click", () => {
    const open = panel.hidden;
    setOpen(open);
    if (open) void refresh();
  });
  createDefault.addEventListener("click", () => void run("vault_create", { useDefault: true }));
  createPick.addEventListener("click", () => void run("vault_create"));
  openPick.addEventListener("click", () => void run("vault_open"));
  backupPick.addEventListener("click", () => void run("vault_backup"));
  for (const button of document.querySelectorAll("[data-manual]")) {
    button.addEventListener("click", () => {
      const value = manualInput.value.trim();
      if (!value) {
        setMessage("パスを入力してください。");
        manualInput.focus();
        return;
      }
      const type = button.dataset.manual;
      void run(type, type === "vault_backup" ? { directory: value } : { path: value });
    });
  }
  approveButton.addEventListener("click", () => {
    if (pendingApproval) void run(pendingApproval.type, pendingApproval.params);
  });
  approveCancel.addEventListener("click", () => {
    approvalBox.hidden = true;
    pendingApproval = null;
    setMessage("キャンセルしました。");
  });

  return {
    refresh,
    open: () => setOpen(true),
    get status() {
      return status;
    },
  };
}
