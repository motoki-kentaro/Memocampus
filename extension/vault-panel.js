// Vault settings section of the side panel. All file access happens in the native host; this
// only sends requests and renders the host's answers.

import { SearchError } from "./search-client.js";
import { messageTable, t } from "./i18n.js";
import {
  BACKUP_FORMAT_NOTE,
  CONVERT_TEXT,
  PUBLICATION_NOTE,
  backupFormat,
  checkBackupPassword,
  checkSpell,
  convertDirection,
  convertMakesSpell,
  reissueOffered,
  describeConversionPending,
  describeConverted,
  describeSpellProblem,
  describePolicyCopy,
  describePublication,
  describeReasons,
  describeRestorePending,
  describeRestoreSource,
  describeRestored,
  describeStatus,
  describeVerdict,
  formatCounts,
  normalizeSpellInput,
  spellMatches,
  texts,
  vaultRequest,
} from "./vault-client.js";

const PUBLICATION_TYPES = new Set(["vault_publication_confirm", "vault_publication_revoke"]);
/** Requests that may open a Windows dialog when no path is given. */
const DIALOG_TYPES = new Set(["vault_create", "vault_open", "vault_backup", "vault_create_encrypted", "vault_recover", "vault_restore_inspect", "vault_restore", "vault_convert_start"]);
/** Issue #51 Slice C: the steps that end a restore. */
const RESTORE_END_TYPES = new Set(["vault_restore_activate", "vault_restore_discard"]);
/** Creation of an encrypted vault ends (either way) with these. */
const CREATION_END_TYPES = new Set(["vault_create_encrypted_commit", "vault_create_encrypted_cancel"]);
/** Issue #51 Slice D: the steps that end a conversion. */
const CONVERT_END_TYPES = new Set(["vault_convert_activate", "vault_convert_keep", "vault_convert_discard"]);
/** Requests about the vault shown, named by its token: refused by the host once another is active. */
const NAMES_SHOWN_TYPES = new Set(["vault_backup", "vault_convert_start"]);

const $ = (selector) => document.querySelector(selector);

/** Labels of the spell step's two buttons, by what the step finishes (create or convert). */
const CONFIRM_LABEL_KEYS = Object.freeze({ create: "vaultSpellConfirmCreate", convert: "vaultSpellConfirmConvert" });
const CANCEL_LABEL_KEYS = Object.freeze({ create: "vaultOnboardingCancelCreate", convert: "vaultOnboardingCancelConvert" });
const confirmLabel = messageTable(CONFIRM_LABEL_KEYS);
const cancelLabel = messageTable(CANCEL_LABEL_KEYS);

/** Puts `text` on the clipboard, only ever from the user's own click on "コピー". */
function writeClipboard(text) {
  return navigator.clipboard.writeText(text);
}

/**
 * `reveal` brings the settings view into sight (the side panel's settings tab); `onStatus` receives
 * every rendered status (null when the host could not be asked). `copyText` writes to the clipboard
 * (replaceable in tests).
 */
export function createVaultPanel({ send, onVaultChanged, reveal = () => {}, onStatus = () => {}, copyText = writeClipboard }) {
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
  // MCP publication of the vault shown (Issue #51): decided here, never through MCP.
  const publicationBox = $("#vault-publication");
  const publicationState = $("#vault-publication-state");
  const publicationNote = $("#vault-publication-note");
  const publicationConfirm = $("#vault-publication-confirm");
  const publicationRevoke = $("#vault-publication-revoke");
  const publicationReview = $("#vault-publication-review");
  const publicationReviewTitle = $("#vault-publication-review-title");
  const publicationApprove = $("#vault-publication-approve");
  const publicationCancel = $("#vault-publication-cancel");
  $("#vault-note").textContent = texts.LOCATION_NOTE;
  // Issue #51: encryption choice, the Recovery Spell (ふっかつのじゅもん) shown once, and recovery.
  const encryptionBox = $("#vault-encryption");
  const encryptionNote = $("#vault-encryption-note");
  const encryptedChoice = $("#vault-encryption-encrypted");
  const plainChoice = $("#vault-encryption-plain");
  const onboardingBox = $("#vault-onboarding");
  const onboardingTitle = $("#vault-onboarding-title");
  const onboardingPath = $("#vault-onboarding-path");
  const spellShow = $("#vault-spell-show");
  const spellText = $("#vault-spell");
  const spellCopy = $("#vault-spell-copy");
  const spellWritten = $("#vault-spell-written");
  const spellCheck = $("#vault-spell-check");
  const spellConfirmInput = $("#vault-spell-confirm-input");
  const spellConfirm = $("#vault-spell-confirm");
  const spellBack = $("#vault-spell-back");
  const onboardingCancel = $("#vault-onboarding-cancel");
  const creationPendingBox = $("#vault-creation-pending");
  const creationPendingText = $("#vault-creation-pending-text");
  const creationDiscard = $("#vault-creation-discard");
  const recoveryOffer = $("#vault-recovery-offer");
  const recoveryStart = $("#vault-recovery-start");
  const recoveryBox = $("#vault-recovery");
  const recoveryTitle = $("#vault-recovery-title");
  const recoveryInput = $("#vault-recovery-input");
  const recoverySubmit = $("#vault-recovery-submit");
  const recoveryCancel = $("#vault-recovery-cancel");
  // Issue #51 Slice C: backup protection and restore. Passwords and spells live only in these
  // inputs and in the one request that carries them; they are cleared when the step ends.
  const backupNormal = $("#vault-backup-normal");
  const backupPassword = $("#vault-backup-password");
  const backupNote = $("#vault-backup-note");
  const backupPasswordFields = $("#vault-backup-password-fields");
  const backupPasswordInput = $("#vault-backup-password-input");
  const backupPasswordConfirm = $("#vault-backup-password-confirm");
  // Review finding C-03: an encrypted vault's backup (B, D) needs the spell that recovers it.
  const backupSpellRow = $("#vault-backup-spell-row");
  const backupSpell = $("#vault-backup-spell");
  $("#vault-backup-spell-note").textContent = texts.BACKUP_SPELL_NOTE;
  const restorePick = $("#vault-restore-pick");
  const restoreBox = $("#vault-restore");
  const restoreTitle = $("#vault-restore-title");
  const restorePasswordRow = $("#vault-restore-password-row");
  const restorePassword = $("#vault-restore-password");
  const restoreSpellRow = $("#vault-restore-spell-row");
  const restoreSpell = $("#vault-restore-spell");
  const restoreSubmit = $("#vault-restore-submit");
  const restoreCancel = $("#vault-restore-cancel");
  const restoredBox = $("#vault-restored");
  const restoredTitle = $("#vault-restored-title");
  const restoredPolicy = $("#vault-restored-policy");
  const restoredPolicyDetails = $("#vault-restored-policy-details");
  const restoredPolicyJson = $("#vault-restored-policy-json");
  const restoreActivate = $("#vault-restore-activate");
  const restoreDiscard = $("#vault-restore-discard");
  // Issue #51 Slice D: converting the vault shown into a new vault of the other mode.
  const convertBox = $("#vault-convert");
  const convertTitle = $("#vault-convert-title");
  const convertNote = $("#vault-convert-note");
  const convertSource = $("#vault-convert-source");
  const convertAckRow = $("#vault-convert-ack-row");
  const convertAck = $("#vault-convert-ack");
  const convertPick = $("#vault-convert-pick");
  const reissueBox = $("#vault-reissue");
  const reissueTitle = $("#vault-reissue-title");
  const reissueNote = $("#vault-reissue-note");
  const reissuePick = $("#vault-reissue-pick");
  const convertedBox = $("#vault-converted");
  const convertedTitle = $("#vault-converted-title");
  const convertActivate = $("#vault-convert-activate");
  const convertKeep = $("#vault-convert-keep");
  const convertDiscard = $("#vault-convert-discard");
  $("#vault-backup-password-note").textContent = texts.PASSWORD_NOTE;
  encryptionNote.textContent = texts.ENCRYPTION_NOTE;
  onboardingTitle.textContent = texts.SPELL_TITLE;
  $("#vault-spell-lead").textContent = texts.SPELL_LEAD;
  $("#vault-spell-caution").textContent = texts.SPELL_CAUTION;
  $("#vault-spell-note").textContent = texts.SPELL_NOTE;
  $("#vault-spell-check-title").textContent = texts.SPELL_CHECK_TITLE;

  const actionButtons = [createDefault, createPick, openPick, backupPick, restorePick, convertPick, reissuePick, publicationConfirm, publicationRevoke, publicationApprove, ...document.querySelectorAll("[data-manual]")];
  const creationButtons = [createDefault, createPick, openPick, document.querySelector('[data-manual="vault_create"]'), document.querySelector('[data-manual="vault_open"]')];
  // The spell lives only here, in this page's memory, until the creation ends; it is never stored
  // (no chrome.storage, no queue, no log) and no request asks the host for it again.
  let onboarding = null;
  let recoveryTarget = null;
  // The backup chosen for a restore (`vault_restore_inspect` answer), and the restored vault waiting
  // for the user's decision (`vault_restore` answer, or `status.restorePending`).
  let restoreSource = null;
  let restored = null;
  // The converted vault waiting for the user's decision (start / confirm answer, or
  // `status.conversionPending`).
  let converted = null;
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
      publicationConfirm.disabled = !backupSource() || status.publication !== "pending";
      publicationApprove.disabled = publicationConfirm.disabled;
      publicationRevoke.disabled = !backupSource() || status.publication !== "confirmed";
    }
    // No other vault is created or opened while a creation waits for its spell confirmation.
    const creationOpen = Boolean(onboarding) || Boolean(status?.creationPending);
    if (creationOpen) for (const button of creationButtons) button.disabled = true;
    const canEncrypt = status?.encryptionAvailable === true;
    encryptedChoice.disabled = busy || !canEncrypt;
    plainChoice.disabled = busy;
    if (!canEncrypt && status) plainChoice.checked = true;
    for (const button of [spellCopy, spellWritten, spellConfirm, spellBack, onboardingCancel, creationDiscard, recoveryStart, recoverySubmit, recoveryCancel, restoreSubmit, restoreCancel, restoreDiscard]) button.disabled = busy;
    // One restore at a time: none starts while a restored vault waits for a decision.
    if (status?.restorePending || restored) restorePick.disabled = true;
    restoreActivate.disabled = busy || !restored?.vaultToken;
    // One vault operation of these at a time (the host refuses the others as well).
    const conversionOpen = Boolean(status?.conversionPending) || Boolean(converted) || onboarding?.kind === "convert";
    if (conversionOpen) restorePick.disabled = true;
    const direction = convertDirection(status?.mode);
    const conversionBlocked =
      busy || !manageable || !backupSource() || conversionOpen || Boolean(status?.restorePending) || Boolean(status?.creationPending) || Boolean(onboarding);
    convertPick.disabled = conversionBlocked || !direction || (direction === "decrypt" && !convertAck.checked);
    reissuePick.disabled = conversionBlocked || !reissueOffered(status?.mode);
    convertAck.disabled = busy;
    for (const button of [convertKeep, convertDiscard]) button.disabled = busy;
    convertActivate.disabled = busy || !converted?.vaultToken;
    convertKeep.disabled = busy || !converted?.vaultToken;
    backupNormal.disabled = busy;
    backupPassword.disabled = busy;
  }

  function protection() {
    return backupPassword.checked ? "password" : "normal";
  }

  function clearBackupPassword() {
    backupPasswordInput.value = "";
    backupPasswordConfirm.value = "";
  }

  /** Forgets every backup secret typed (password and spell). */
  function clearBackupSecrets() {
    clearBackupPassword();
    backupSpell.value = "";
  }

  function encryptedShown() {
    return status?.mode === "encrypted";
  }

  /** The parameters of a backup as chosen, or null (with a message) when the password is not usable. */
  function backupParams(extra = {}) {
    const params = { ...extra, protection: protection() };
    if (params.protection === "password") {
      const problem = checkBackupPassword(backupPasswordInput.value, backupPasswordConfirm.value);
      if (problem) {
        setMessage(`${problem}\n${t("vaultBackupNotCreated")}`);
        backupPasswordInput.focus();
        return null;
      }
      params.password = backupPasswordInput.value;
    }
    if (encryptedShown()) {
      const problem = checkSpell(backupSpell.value);
      if (problem) {
        setMessage(`${problem}\n${t("vaultBackupNotCreated")}`);
        backupSpell.focus();
        return null;
      }
      params.recoverySpell = normalizeSpellInput(backupSpell.value);
    }
    return params;
  }

  function encryptedChosen() {
    return encryptedChoice.checked && status?.encryptionAvailable === true;
  }

  function createType() {
    return encryptedChosen() ? "vault_create_encrypted" : "vault_create";
  }

  /** The vault a backup or a publication decision is about: the one shown, if it is ready and named by a token. */
  function backupSource() {
    if (status?.state !== "ready" || typeof status.vaultToken !== "string" || !/^[0-9a-f]{64}$/.test(status.vaultToken)) return null;
    return { vaultPath: status.vaultPath, vaultToken: status.vaultToken };
  }

  function render() {
    // The buttons follow the status even if a message cannot be shown (a missing catalog entry
    // throws): nothing is left enabled by a half-finished render.
    try {
      renderText();
    } finally {
      updateButtons();
    }
    onStatus(status);
  }

  function renderText() {
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
      const verdict = describeVerdict(location.verdict);
      verdictText.textContent = location.verdict === "needs_approval" && status.locationApproved ? t("vaultVerdictApproved", verdict) : verdict;
      fillList(reasonsList, describeReasons(location.reasons));
    }

    const publication = status?.publication;
    // Hidden for an older host (no field) and wherever MCP cannot read anything (null).
    publicationBox.hidden = publication !== "confirmed" && publication !== "pending";
    if (!publicationBox.hidden) {
      publicationBox.dataset.state = publication;
      publicationState.textContent = describePublication(status);
      const usable = status.state === "ready";
      publicationNote.textContent = publication === "pending" && !usable ? `${PUBLICATION_NOTE.pending}\n${PUBLICATION_NOTE.unusable}` : PUBLICATION_NOTE[publication];
      publicationConfirm.hidden = publication !== "pending" || !usable;
      publicationRevoke.hidden = publication !== "confirmed";
    }
    if (publication !== "pending") publicationReview.hidden = true;

    encryptionBox.hidden = !status?.managementEnabled;
    backupNote.textContent = BACKUP_FORMAT_NOTE[backupFormat(status?.mode, protection())];
    backupPasswordFields.hidden = protection() !== "password";
    backupSpellRow.hidden = !encryptedShown();
    if (backupSpellRow.hidden) backupSpell.value = "";
    // A restore recorded by the host, but not shown in this page (e.g. the panel was reopened).
    const pending = status?.restorePending ?? null;
    if (!pending && restored?.fromStatus) closeRestored();
    if (pending && !restored) showRestorePending(pending);
    const pendingPath = typeof status?.creationPending === "string" ? status.creationPending : null;
    creationPendingBox.hidden = pendingPath === null || Boolean(onboarding);
    if (!creationPendingBox.hidden) {
      creationPendingText.textContent = `${t("vaultCreationPending")}${pendingPath ? `\n${t("vaultLocationLine", String(pendingPath))}` : ""}`;
    }
    recoveryOffer.hidden = !(status?.mode === "encrypted" && status.recoveryAvailable === true && status.state !== "ready");

    // Slice D: offered only by a host that knows conversions (the field exists), for a ready vault.
    const direction = convertDirection(status?.mode);
    const offered = Boolean(status?.managementEnabled) && status?.state === "ready" && direction !== null && status !== null && "conversionPending" in status;
    convertBox.hidden = !offered;
    if (offered) {
      if (convertBox.dataset.direction !== direction) convertAck.checked = false;
      convertBox.dataset.direction = direction;
      const text = CONVERT_TEXT[direction];
      convertTitle.textContent = text.title;
      convertNote.textContent = text.note;
      convertPick.textContent = text.pick;
      convertSource.textContent = t("vaultConvertSourceLine", String(status.vaultPath));
      convertAckRow.hidden = direction !== "decrypt";
    } else {
      convertAck.checked = false;
    }
    // Slice E: an encrypted vault's spell can be re-issued (a new vault under a new spell).
    reissueBox.hidden = !(offered && reissueOffered(status?.mode));
    if (!reissueBox.hidden) {
      reissueTitle.textContent = CONVERT_TEXT.reissue.title;
      reissueNote.textContent = CONVERT_TEXT.reissue.note;
      reissuePick.textContent = CONVERT_TEXT.reissue.pick;
    }
    const conversion = status?.conversionPending ?? null;
    if (!conversion && converted?.fromStatus) closeConverted();
    if (conversion && !converted && !onboarding) showConversionPending(conversion);

    if (status?.defaultPath) {
      defaultPathText.textContent = status.defaultExists ? t("vaultDefaultExists", String(status.defaultPath)) : status.defaultPath;
    } else {
      defaultPathText.textContent = "";
    }
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
        setMessage(error instanceof SearchError ? error.message : t("vaultStatusFailed"));
      }
      render();
      return status;
    })();
    latestStatus = answered;
    return answered;
  }

  function showApproval(type, params, response) {
    pendingApproval = { type, params: { ...params, path: response.path, approveWarnings: true } };
    approvalTitle.textContent = t("vaultLocationWarning", String(response.path));
    fillList(approvalReasons, describeReasons(response.location?.reasons));
    approvalBox.hidden = false;
    approveButton.focus();
  }

  function successMessage(type, response) {
    if (type === "vault_backup") {
      const created =
        typeof response.format === "string"
          ? t("vaultBackupCreatedFormat", response.format, String(response.vaultPath), String(response.backupPath))
          : t("vaultBackupCreated", String(response.vaultPath), String(response.backupPath));
      return response.policyCopy === true ? `${created}\n${t("vaultBackupPolicyIncluded")}` : created;
    }
    if (type === "vault_restore_activate") return t("vaultRestoreActivated", String(response.path));
    if (type === "vault_restore_discard") return t("vaultRestoreDiscarded");
    if (type === "vault_publication_confirm") return t("vaultPublicationApproved");
    if (type === "vault_publication_revoke") return t("vaultPublicationRevoked");
    if (type === "vault_create") return t("vaultCreated", String(response.path));
    if (type === "vault_create_encrypted_commit") return t("vaultEncryptedCreated", String(response.path));
    if (type === "vault_create_encrypted_cancel") return t("vaultCreationDiscarded");
    if (type === "vault_recover") return t("vaultRecovered", String(response.path));
    if (type === "vault_convert_activate") return t("vaultConvertActivated", String(response.path));
    if (type === "vault_convert_keep") return t("vaultConvertKept", String(response.path));
    if (type === "vault_convert_discard") return t("vaultConvertDiscarded");
    return t("vaultOpened", String(response.path));
  }

  async function run(type, params = {}) {
    if (busy) return;
    // A backup (Issue #48) and a publication decision (Issue #51) name the vault shown: the host
    // refuses them if another vault is active.
    const isPublication = PUBLICATION_TYPES.has(type);
    const namesShown = NAMES_SHOWN_TYPES.has(type) || isPublication;
    const source = namesShown ? backupSource() : null;
    if (namesShown && !source) {
      setMessage(t(isPublication ? "vaultPublicationSourceUnknown" : type === "vault_convert_start" ? "vaultConvertSourceUnknown" : "vaultBackupSourceUnknown"));
      await refresh();
      return;
    }
    if (source) params = { ...params, expectedVaultToken: source.vaultToken };
    busy = true;
    approvalBox.hidden = true;
    publicationReview.hidden = true;
    pendingApproval = null;
    updateButtons();
    const usesDialog = DIALOG_TYPES.has(type) && !params.path && !params.directory && !params.useDefault;
    try {
      // Inside the try: a missing message sends nothing and still ends the step (busy, secrets, acknowledgement).
      setMessage(t(usesDialog ? "vaultChooseInDialog" : "vaultWorking"));
      const response = await vaultRequest(type, params, { send });
      if (type === "vault_backup" && response.vaultToken !== source.vaultToken) {
        // The host only backs up the vault named; an answer naming another is reported, not trusted.
        setMessage(t("vaultBackupSourceMismatch", String(response.vaultPath), String(response.backupPath)));
        return;
      }
      if (type === "vault_create_encrypted") {
        // Nothing is active yet: the creation ends only after the whole spell is entered again.
        startOnboarding(response);
        return;
      }
      if (type === "vault_restore_inspect") {
        startRestore(response);
        return;
      }
      if (type === "vault_restore") {
        // Restored, but not active: the user decides after seeing what was restored.
        closeRestore();
        showRestored(response);
        setMessage(t("vaultRestoreDone"));
        return;
      }
      if (type === "vault_convert_start") {
        // Converted and verified, nothing active changed. D1 and re-issue: the new spell, once,
        // then its re-entry; D2: the decision right away.
        if (convertMakesSpell(response.direction)) startOnboarding(response, "convert");
        else {
          showConverted(response);
          setMessage(t("vaultDecryptCreated"));
        }
        return;
      }
      if (type === "vault_convert_confirm") {
        const shown = { ...response, direction: onboarding?.direction ?? "encrypt", sourcePath: onboarding?.sourcePath, counts: onboarding?.counts };
        clearOnboarding();
        showConverted(shown);
        setMessage(t("vaultConvertSpellConfirmed"));
        return;
      }
      if (RESTORE_END_TYPES.has(type)) closeRestored();
      if (CREATION_END_TYPES.has(type)) clearOnboarding();
      if (CONVERT_END_TYPES.has(type)) {
        closeConverted();
        if (onboarding?.kind === "convert") clearOnboarding();
      }
      if (type === "vault_recover") closeRecovery();
      setMessage(successMessage(type, response));
      if (response.status) showStatus(response.status);
      const unchanged = ["vault_backup", "vault_create_encrypted_cancel", "vault_restore_discard", "vault_convert_keep", "vault_convert_discard"];
      if (!isPublication && !unchanged.includes(type)) onVaultChanged();
    } catch (error) {
      const response = error instanceof SearchError ? error.response : null;
      if (type === "vault_create_encrypted_commit" && (error.code === "vault_creation_not_pending" || error.code === "vault_creation_damaged")) clearOnboarding();
      if (type === "vault_restore_activate" && error.code === "vault_restore_not_pending") closeRestored();
      if (type === "vault_convert_confirm" && (error.code === "vault_convert_not_pending" || error.code === "vault_convert_damaged" || error.code === "vault_convert_invalid_state")) clearOnboarding();
      if (CONVERT_END_TYPES.has(type) && error.code === "vault_convert_not_pending") closeConverted();
      if (error.code === "recovery_spell_invalid") {
        setMessage(describeSpellProblem(response));
      } else if ((error.code === "key_unavailable" || error.code === "vault_locked" || error.code === "vault_decrypt_failed") && response?.recoveryAvailable === true && response.path) {
        setMessage(`${error.message}\n${t("vaultRecoverableHint")}`);
        startRecovery(response.path);
      } else if (source && error.code === "vault_switched") {
        const switched = isPublication ? "vaultSwitchedPublication" : type === "vault_convert_start" ? "vaultSwitchedConvert" : "vaultSwitchedBackup";
        setMessage(t(switched, String(source.vaultPath)));
      } else if (isPublication && typeof error.code === "string" && error.code.startsWith("policy_")) {
        setMessage(t("vaultPublicationPolicyUnreadable"));
      } else if (error.code === "location_needs_approval" && response?.path) {
        setMessage("");
        showApproval(type, params, response);
      } else {
        const reasons = describeReasons(response?.location?.reasons);
        const lines = [error instanceof SearchError ? error.message : t("errorGeneric")];
        if (response?.created) lines.push(t("vaultCreatedNotActive"));
        if (response?.path) lines.push(response.path);
        if (reasons.length) lines.push(...reasons.map((reason) => t("vaultReasonBullet", String(reason))));
        setMessage(lines.join("\n"));
      }
    } finally {
      // A backup password and spell are used for one request only.
      if (type === "vault_backup") clearBackupSecrets();
      // The acknowledgement of a plaintext copy is given for one request only.
      if (type === "vault_convert_start") convertAck.checked = false;
      busy = false;
      await refresh();
    }
  }

  function startRestore(info) {
    restoreSource = info;
    restoreTitle.textContent = describeRestoreSource(info);
    restorePasswordRow.hidden = info.needsPassword !== true;
    restoreSpellRow.hidden = info.needsSpell !== true;
    restorePassword.value = "";
    restoreSpell.value = "";
    restoreBox.hidden = false;
    setMessage("");
    (info.needsPassword ? restorePassword : info.needsSpell ? restoreSpell : restoreSubmit).focus();
  }

  /** Forgets the backup chosen and the secrets typed for it. */
  function closeRestore() {
    restoreSource = null;
    restorePassword.value = "";
    restoreSpell.value = "";
    restoreBox.hidden = true;
  }

  function showRestored(response) {
    restored = { path: response.path, vaultToken: response.vaultToken, fromStatus: false };
    restoredTitle.textContent = describeRestored(response);
    restoredPolicy.textContent = describePolicyCopy(response.policyCopy);
    const copy = response.policyCopy;
    restoredPolicyDetails.hidden = copy?.state !== "valid";
    restoredPolicyJson.textContent = copy?.state === "valid" ? JSON.stringify({ policy: copy.policy, approvedDomains: copy.approvedDomains }, null, 2) : "";
    restoreActivate.hidden = false;
    restoredBox.hidden = false;
    restoreActivate.focus();
  }

  function showRestorePending(pending) {
    const usable = pending.phase === "restored" || pending.phase === "verified";
    restored = { path: pending.path, vaultToken: usable ? pending.vaultToken : null, fromStatus: true };
    restoredTitle.textContent = describeRestorePending(pending);
    restoredPolicy.textContent = "";
    restoredPolicyDetails.hidden = true;
    restoredPolicyJson.textContent = "";
    restoreActivate.hidden = !usable;
    restoredBox.hidden = false;
  }

  function closeRestored() {
    restored = null;
    restoredPolicyJson.textContent = "";
    restoredBox.hidden = true;
  }

  function showConverted(response) {
    converted = { path: response.path, vaultToken: response.vaultToken ?? null, fromStatus: false };
    convertedTitle.textContent = describeConverted(response);
    convertActivate.hidden = false;
    convertKeep.hidden = false;
    convertedBox.hidden = false;
    convertActivate.focus();
  }

  function showConversionPending(pending) {
    // Usable: converted, or (decrypt) verified, which the host promotes on activation. A D1
    // conversion interrupted before its spell was confirmed can only be discarded here: the spell
    // is never shown again.
    const usable = pending.phase === "converted" || (pending.phase === "verified" && pending.direction === "decrypt");
    converted = { path: pending.path, vaultToken: usable ? pending.vaultToken : null, fromStatus: true };
    convertedTitle.textContent = describeConversionPending(pending);
    convertActivate.hidden = !usable;
    convertKeep.hidden = !usable;
    convertedBox.hidden = false;
  }

  function closeConverted() {
    converted = null;
    convertedBox.hidden = true;
  }

  function showSpell() {
    spellText.textContent = onboarding.spell;
    spellShow.hidden = false;
    spellCheck.hidden = true;
    spellConfirmInput.value = "";
  }

  /** `kind`: "create" (a new encrypted vault) or "convert" (Slice D: a plain vault encrypted). */
  function startOnboarding(response, kind = "create") {
    const spell = typeof response?.recoverySpell === "string" ? response.recoverySpell : "";
    if (checkSpell(spell) !== null || spell !== normalizeSpellInput(spell)) {
      setMessage(t(kind === "convert" ? "vaultSpellNotReceivedConvert" : "vaultSpellNotReceivedCreate"));
      return;
    }
    // Every text is read before the step starts: a missing message stops here, with nothing shown.
    const labels = { confirm: confirmLabel[kind], cancel: cancelLabel[kind] };
    const pathText =
      kind === "convert"
        ? `${t("vaultOnboardingConvertPaths", String(response.path), String(response.sourcePath ?? ""))}\n${texts.CONVERT_SPELL_NOTE}${response.direction === "reissue" ? `\n${texts.REISSUE_SPELL_NOTE}` : ""}`
        : t("vaultLocationLine", String(response.path));
    onboarding = { spell, path: response.path, kind, direction: response.direction ?? null, sourcePath: response.sourcePath ?? null, counts: response.counts ?? null };
    spellConfirm.textContent = labels.confirm;
    onboardingCancel.textContent = labels.cancel;
    onboardingPath.textContent = pathText;
    showSpell();
    onboardingBox.hidden = false;
    setMessage("");
    spellWritten.focus();
  }

  /** Forgets the spell: memory, DOM and the confirmation input. */
  function clearOnboarding() {
    // The spell leaves the page first: a missing message for the labels below must not leave it shown.
    onboarding = null;
    spellText.textContent = "";
    spellConfirmInput.value = "";
    onboardingPath.textContent = "";
    spellShow.hidden = false;
    spellCheck.hidden = true;
    onboardingBox.hidden = true;
    spellConfirm.textContent = confirmLabel.create;
    onboardingCancel.textContent = cancelLabel.create;
  }

  function startRecovery(path) {
    recoveryTarget = path;
    recoveryTitle.textContent = `${texts.RECOVERY_TITLE}\n${t("vaultTargetLine", String(path))}`;
    recoveryInput.value = "";
    recoveryBox.hidden = false;
    recoveryInput.focus();
  }

  function closeRecovery() {
    recoveryTarget = null;
    recoveryInput.value = "";
    recoveryBox.hidden = true;
  }

  spellCopy.addEventListener("click", async () => {
    if (!onboarding) return;
    try {
      await copyText(onboarding.spell);
      setMessage(t("vaultSpellCopied"));
    } catch {
      setMessage(t("vaultSpellCopyFailed"));
    }
  });
  spellWritten.addEventListener("click", () => {
    if (!onboarding) return;
    // The spell leaves the page while the user enters it again from where they kept it.
    spellText.textContent = "";
    spellShow.hidden = true;
    spellConfirmInput.value = "";
    spellCheck.hidden = false;
    spellConfirmInput.focus();
  });
  spellBack.addEventListener("click", () => {
    if (onboarding) showSpell();
  });
  spellConfirm.addEventListener("click", () => {
    if (!onboarding || busy) return;
    const typed = spellConfirmInput.value;
    const converting = onboarding.kind === "convert";
    if (!spellMatches(onboarding.spell, typed)) {
      const problem = checkSpell(typed);
      setMessage(`${problem ?? t("vaultSpellMismatch")}\n${t(converting ? "vaultSpellRetryConvert" : "vaultSpellRetryCreate")}`);
      spellConfirmInput.focus();
      return;
    }
    void run(converting ? "vault_convert_confirm" : "vault_create_encrypted_commit", { recoverySpell: onboarding.spell });
  });
  onboardingCancel.addEventListener("click", () => void run(onboarding?.kind === "convert" ? "vault_convert_discard" : "vault_create_encrypted_cancel"));
  creationDiscard.addEventListener("click", () => void run("vault_create_encrypted_cancel"));
  recoveryStart.addEventListener("click", () => {
    if (status?.vaultPath) startRecovery(status.vaultPath);
  });
  recoverySubmit.addEventListener("click", () => {
    if (!recoveryTarget || busy) return;
    const problem = checkSpell(recoveryInput.value);
    if (problem) {
      setMessage(`${problem}\n${t("vaultRecoveryUnchanged")}`);
      recoveryInput.focus();
      return;
    }
    void run("vault_recover", { path: recoveryTarget, recoverySpell: normalizeSpellInput(recoveryInput.value) });
  });
  recoveryCancel.addEventListener("click", () => {
    closeRecovery();
    setMessage(t("vaultRecoveryCancelled"));
  });

  createDefault.addEventListener("click", () => void run(createType(), { useDefault: true }));
  createPick.addEventListener("click", () => void run(createType()));
  openPick.addEventListener("click", () => void run("vault_open"));
  backupPick.addEventListener("click", () => {
    const params = backupParams();
    if (params) void run("vault_backup", params);
  });
  for (const choice of [backupNormal, backupPassword]) {
    choice.addEventListener("change", () => {
      if (!backupPassword.checked) clearBackupPassword();
      render();
    });
  }
  restorePick.addEventListener("click", () => void run("vault_restore_inspect"));
  restoreSubmit.addEventListener("click", () => {
    if (!restoreSource || busy) return;
    const params = { backupPath: restoreSource.backupPath };
    if (restoreSource.needsPassword) {
      if (!restorePassword.value) {
        setMessage(t("vaultRestorePasswordRequired"));
        restorePassword.focus();
        return;
      }
      params.password = restorePassword.value;
    }
    if (restoreSource.needsSpell) {
      const problem = checkSpell(restoreSpell.value);
      if (problem) {
        setMessage(`${problem}\n${t("vaultNothingRestored")}`);
        restoreSpell.focus();
        return;
      }
      params.recoverySpell = normalizeSpellInput(restoreSpell.value);
    }
    void run("vault_restore", params);
  });
  restoreCancel.addEventListener("click", () => {
    closeRestore();
    setMessage(t("vaultRestoreCancelled"));
  });
  restoreActivate.addEventListener("click", () => {
    if (restored?.vaultToken) void run("vault_restore_activate", { expectedVaultToken: restored.vaultToken });
  });
  restoreDiscard.addEventListener("click", () => void run("vault_restore_discard"));
  convertAck.addEventListener("change", () => updateButtons());
  convertPick.addEventListener("click", () => {
    const direction = convertDirection(status?.mode);
    if (!direction || busy) return;
    if (direction === "decrypt" && !convertAck.checked) {
      setMessage(t("vaultDecryptNotAcknowledged"));
      return;
    }
    const params = { direction };
    if (direction === "decrypt") params.acknowledgePlaintext = true;
    void run("vault_convert_start", params);
  });
  reissuePick.addEventListener("click", () => {
    if (busy || !reissueOffered(status?.mode)) return;
    void run("vault_convert_start", { direction: "reissue" });
  });
  convertActivate.addEventListener("click", () => {
    if (converted?.vaultToken) void run("vault_convert_activate", { expectedVaultToken: converted.vaultToken });
  });
  convertKeep.addEventListener("click", () => {
    if (converted?.vaultToken) void run("vault_convert_keep");
  });
  convertDiscard.addEventListener("click", () => void run("vault_convert_discard"));
  for (const button of document.querySelectorAll("[data-manual]")) {
    button.addEventListener("click", () => {
      const value = manualInput.value.trim();
      if (!value) {
        setMessage(t("vaultEnterPath"));
        manualInput.focus();
        return;
      }
      const type = button.dataset.manual === "vault_create" ? createType() : button.dataset.manual;
      if (type === "vault_backup") {
        const params = backupParams({ directory: value });
        if (params) void run(type, params);
        return;
      }
      void run(type, { path: value });
    });
  }
  approveButton.addEventListener("click", () => {
    if (pendingApproval) void run(pendingApproval.type, pendingApproval.params);
  });
  approveCancel.addEventListener("click", () => {
    approvalBox.hidden = true;
    // A recovery waiting for this approval carries the spell: forget it with the request.
    pendingApproval = null;
    setMessage(t("vaultCancelled"));
  });
  // Confirming takes two steps: what publishing means and which vault, then the approval itself.
  publicationConfirm.addEventListener("click", () => {
    const source = backupSource();
    if (busy || !source || status?.publication !== "pending") return;
    publicationReviewTitle.textContent = `${texts.PUBLICATION_REVIEW}\n${t("vaultTargetLine", String(source.vaultPath))}`;
    publicationReview.hidden = false;
    publicationApprove.focus();
  });
  publicationApprove.addEventListener("click", () => void run("vault_publication_confirm"));
  publicationCancel.addEventListener("click", () => {
    publicationReview.hidden = true;
    setMessage(t("vaultPublicationCancelled"));
  });
  publicationRevoke.addEventListener("click", () => void run("vault_publication_revoke"));

  return {
    refresh,
    open: reveal,
    get status() {
      return status;
    },
  };
}
