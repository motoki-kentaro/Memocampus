// Vault settings section of the side panel. All file access happens in the native host; this
// only sends requests and renders the host's answers.

import { SearchError } from "./search-client.js";
import { LOCATION_NOTE, VERDICT_TEXT, describeReasons, describeStatus, formatCounts, vaultRequest } from "./vault-client.js";

const $ = (selector) => document.querySelector(selector);

/**
 * `reveal` brings the settings view into sight (the side panel's settings tab); `onStatus` receives
 * every rendered status (null when the host could not be asked).
 */
export function createVaultPanel({ send, onVaultChanged, reveal = () => {}, onStatus = () => {} }) {
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
  // Each status asked for; only the latest answer is shown (an older one arriving late never
  // replaces it, so the vault shown and the token a backup sends always come from one answer).
  let statusRequest = 0;
  // Resolves with the status shown once the latest request is answered: the caller of a superseded
  // request gets the newer status, never the older one it asked for.
  let latestStatus = Promise.resolve(null);
  let busy = false;
  let pendingApproval = null;

  function fillList(list, items) {
    list.replaceChildren(...items.map((text) => Object.assign(document.createElement("li"), { textContent: text })));
  }

  function updateButtons() {
    const manageable = status?.managementEnabled === true;
    for (const button of actionButtons) button.disabled = busy || !manageable;
    if (!busy && manageable) {
      createDefault.disabled = status.defaultExists === true;
      backupPick.disabled = !backupSource();
      document.querySelector('[data-manual="vault_backup"]').disabled = !backupSource();
    }
  }

  /** The vault a backup would be of: the one shown, if it is ready and named by a token. */
  function backupSource() {
    if (status?.state !== "ready" || typeof status.vaultToken !== "string" || !/^[0-9a-f]{64}$/.test(status.vaultToken)) return null;
    return { vaultPath: status.vaultPath, vaultToken: status.vaultToken };
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
    onStatus(status);
  }

  function setMessage(text) {
    messageText.textContent = text;
  }

  /** Shows `value` as the status now; anything asked for before is older than it. */
  function showStatus(value) {
    statusRequest += 1;
    latestStatus = Promise.resolve(value);
    status = value;
    render();
  }

  function refresh() {
    const request = ++statusRequest;
    const answered = (async () => {
      try {
        const response = await vaultRequest("vault_status", {}, { send });
        if (request !== statusRequest) return latestStatus;
        status = response.status || null;
      } catch (error) {
        if (request !== statusRequest) return latestStatus;
        status = null;
        setMessage(error instanceof SearchError ? error.message : "Vaultの状態を取得できませんでした。");
      }
      render();
      return status;
    })();
    latestStatus = answered;
    return answered;
  }

  function showApproval(type, params, response) {
    pendingApproval = { type, params: { ...params, path: response.path, approveWarnings: true } };
    approvalTitle.textContent = `この場所をActive Vaultにすると、同期による破損やデータ消失の恐れがあります:\n${response.path}`;
    fillList(approvalReasons, describeReasons(response.location?.reasons));
    approvalBox.hidden = false;
    approveButton.focus();
  }

  function successMessage(type, response) {
    if (type === "vault_backup") return `バックアップを作成しました。\nバックアップ元: ${response.vaultPath}\n保存先: ${response.backupPath}`;
    if (type === "vault_create") return `新しいVaultを作成し、使用を開始しました:\n${response.path}`;
    return `Vaultを開きました:\n${response.path}`;
  }

  async function run(type, params = {}) {
    if (busy) return;
    // A backup names the vault shown (Issue #48): the host refuses it if another vault is active.
    const source = type === "vault_backup" ? backupSource() : null;
    if (type === "vault_backup" && !source) {
      setMessage("バックアップするVaultを確認できないため、開始しませんでした。表示を更新します。");
      await refresh();
      return;
    }
    if (source) params = { ...params, expectedVaultToken: source.vaultToken };
    busy = true;
    approvalBox.hidden = true;
    pendingApproval = null;
    updateButtons();
    const usesDialog = !params.path && !params.directory && !params.useDefault;
    setMessage(usesDialog ? "Windowsのダイアログで選択してください。\n表示されない場合はタスクバーを確認してください。" : "処理中…");
    try {
      const response = await vaultRequest(type, params, { send });
      if (source && response.vaultToken !== source.vaultToken) {
        // The host only backs up the vault named; an answer naming another is reported, not trusted.
        setMessage(`バックアップ元が表示中のVaultと一致しません。作成されたファイルを確認してください。\nバックアップ元: ${response.vaultPath}\n保存先: ${response.backupPath}`);
        return;
      }
      setMessage(successMessage(type, response));
      if (response.status) showStatus(response.status);
      if (type !== "vault_backup") onVaultChanged();
    } catch (error) {
      const response = error instanceof SearchError ? error.response : null;
      if (source && error.code === "vault_switched") {
        setMessage(`表示していたVault（${source.vaultPath}）は、もうActiveではありません。バックアップは作成していません。\n表示を最新の状態に更新しました。確認してから、もう一度実行してください。`);
      } else if (error.code === "location_needs_approval" && response?.path) {
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
    open: reveal,
    get status() {
      return status;
    },
  };
}
