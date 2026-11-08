// The calendar of visit days above the URL tab's list (Issue #5, Slice 2).
//
// Days are the person's local days. Their starts are computed here, in the browser's time zone, as
// `new Date(year, monthIndex, day)`: the next day starts at `new Date(year, monthIndex, day + 1)`,
// never 24 hours later (a day may last 23, 23.5, 24.5 or 25 hours, or start at 01:00 where midnight
// does not exist). The host knows no time zone. Calendar arithmetic on civil dates (days in a month,
// the weekday of a date) uses UTC so that it cannot depend on the time zone at all.
//
// The popover asks the host for the visits per day of the month it shows, each time it shows one
// (no cache), and keeps an answer only when it is the latest request's and names the vault of the
// list (`getVaultToken`): an answer of another vault is never shown as this one's.

import { SearchError, VAULT_UNAVAILABLE_CODES, visitDays } from "./search-client.js";

export const WEEKDAYS = ["日", "月", "火", "水", "木", "金", "土"];
/** The earliest year offered (`new Date(y, …)` reads years below 100 as 19xx; the host takes 0000–9999). */
export const MIN_YEAR = 1970;
/** How far ahead of this year the calendar goes: a little future is harmless, a far one is useless. */
export const YEARS_AHEAD = 1;

const pad = (n) => String(n).padStart(2, "0");

/** Days in a month of the civil calendar (monthIndex 0–11, as in JavaScript). */
export function daysInMonth(year, monthIndex) {
  return new Date(Date.UTC(year, monthIndex + 1, 0)).getUTCDate();
}

/** The weekday (0 = Sunday) of a civil date, the same in every time zone. */
export function weekdayOf(year, monthIndex, day) {
  return new Date(Date.UTC(year, monthIndex, day)).getUTCDay();
}

/** The month `delta` months from the given one. */
export function addMonths(year, monthIndex, delta) {
  const total = year * 12 + monthIndex + delta;
  return { year: Math.floor(total / 12), monthIndex: ((total % 12) + 12) % 12 };
}

/** Today in the browser's time zone. */
export function localToday(now = new Date()) {
  return { year: now.getFullYear(), monthIndex: now.getMonth(), day: now.getDate() };
}

/** `YYYY/MM/DD` of a local date. */
export function formatDate(year, monthIndex, day) {
  return `${String(year).padStart(4, "0")}/${pad(monthIndex + 1)}/${pad(day)}`;
}

/** The UTC instants of a local day's start and of the next day's start (`search_pages`). */
export function localDayBounds(year, monthIndex, day) {
  return {
    visitedFrom: new Date(year, monthIndex, day).toISOString(),
    visitedTo: new Date(year, monthIndex, day + 1).toISOString(),
  };
}

/** The starts of every local day of a month and of the next month's first day (`visit_days`, 29–32 texts). */
export function monthBounds(year, monthIndex) {
  return Array.from({ length: daysInMonth(year, monthIndex) + 1 }, (_, i) => new Date(year, monthIndex, i + 1).toISOString());
}

/** The weeks of a month, Sunday first: each 7 day numbers or null outside the month (4 to 6 weeks). */
export function calendarWeeks(year, monthIndex) {
  const first = weekdayOf(year, monthIndex, 1);
  const days = daysInMonth(year, monthIndex);
  const weeks = [];
  for (let start = 1 - first; start <= days; start += 7) {
    weeks.push(Array.from({ length: 7 }, (_, i) => (start + i >= 1 && start + i <= days ? start + i : null)));
  }
  return weeks;
}

/** The accessible name of a day: 「11月8日（日）閲覧12件」, and whether it is today or chosen. */
export function dayLabel(year, monthIndex, day, count, { today = false, selected = false } = {}) {
  const visits = count === null ? "読み込み中" : count > 0 ? `閲覧${count}件` : "閲覧なし";
  const notes = [today ? "今日" : null, selected ? "選択中" : null].filter(Boolean);
  return `${monthIndex + 1}月${day}日（${WEEKDAYS[weekdayOf(year, monthIndex, day)]}）${visits}${notes.map((note) => `、${note}`).join("")}`;
}

/**
 * The day the keyboard moves to from `day`, among the selectable days (`enabled(day)`), or null to stay.
 * Left/Right: the previous/next selectable day; Up/Down: the same weekday of earlier/later weeks; Home/End:
 * the first/last selectable day of the same week. The month's edges are not crossed (◀ ▶ change months).
 */
export function moveDay(year, monthIndex, day, key, enabled) {
  const days = daysInMonth(year, monthIndex);
  const column = weekdayOf(year, monthIndex, day);
  const weekStart = Math.max(1, day - column);
  const weekEnd = Math.min(days, day + 6 - column);
  const scan = (from, step, until) => {
    for (let candidate = from; step > 0 ? candidate <= until : candidate >= until; candidate += step) if (enabled(candidate)) return candidate;
    return null;
  };
  switch (key) {
    case "ArrowRight": return scan(day + 1, 1, days);
    case "ArrowLeft": return scan(day - 1, -1, 1);
    case "ArrowDown": return scan(day + 7, 7, days);
    case "ArrowUp": return scan(day - 7, -7, 1);
    case "Home": return day > weekStart ? scan(weekStart, 1, day - 1) : null;
    case "End": return day < weekEnd ? scan(weekEnd, -1, day + 1) : null;
    default: return null;
  }
}

/**
 * The popover. `launcher` opens and closes it; `popover` is the (empty) dialog element it is built in.
 * `getVaultToken()` is the vault of the list (null: not known), `getSelected()` the chosen day (or null),
 * `onSelect({ year, monthIndex, day, vaultToken })` is called when a day is chosen, `onVaultMismatch()`
 * when the host counted in another vault than the list's, `onVaultUnavailable()` for a missing vault, and
 * `fallbackFocus()` when focus must leave a closing popover while the launcher is disabled.
 */
export function createVisitCalendar({ launcher, popover, send, getVaultToken, getSelected, onSelect, onVaultMismatch, onVaultUnavailable = () => {}, fallbackFocus = () => {}, now = () => new Date() }) {
  const doc = popover.ownerDocument;
  const make = (tag, className = "", text = "") => {
    const element = doc.createElement(tag);
    if (className) element.className = className;
    if (text) element.textContent = text;
    return element;
  };
  const button = (className, text = "") => {
    const element = make("button", className, text);
    element.type = "button";
    return element;
  };

  const head = make("div", "calendar-head");
  const prev = button("calendar-nav calendar-prev", "◀");
  const title = button("calendar-title");
  const next = button("calendar-nav calendar-next", "▶");
  head.append(prev, title, next);

  const grid = make("table", "calendar-grid");
  grid.setAttribute("role", "grid");
  const weekdayRow = make("tr");
  for (const [index, name] of WEEKDAYS.entries()) {
    const th = make("th", index === 0 ? "is-sun" : index === 6 ? "is-sat" : "", name);
    th.setAttribute("scope", "col");
    th.setAttribute("abbr", `${name}曜日`);
    weekdayRow.append(th);
  }
  const thead = make("thead");
  thead.append(weekdayRow);
  const tbody = make("tbody");
  grid.append(thead, tbody);

  const months = make("div", "calendar-months");
  months.hidden = true;
  const monthButtons = Array.from({ length: 12 }, (_, monthIndex) => {
    const choice = button("calendar-month", `${monthIndex + 1}月`);
    choice.dataset.month = String(monthIndex);
    months.append(choice);
    return choice;
  });

  const status = make("p", "calendar-status");
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  const retry = button("secondary calendar-retry", "再読み込み");
  retry.hidden = true;
  popover.replaceChildren(head, grid, months, status, retry);

  let open = false;
  let view = "days"; // or "months"
  let shown = localToday(now()); // { year, monthIndex } of the days view
  let pickerYear = shown.year;
  let counts = null; // { year, monthIndex, vaultToken, values } of the shown month, or null
  let generation = 0;
  let focusDay = null; // the day that holds the roving tab stop

  const maxYear = () => localToday(now()).year + YEARS_AHEAD;
  const inRange = (year) => year >= MIN_YEAR && year <= maxYear();
  const countOf = (day) => (counts && counts.year === shown.year && counts.monthIndex === shown.monthIndex ? counts.values[day - 1] : null);
  const selectable = (day) => (countOf(day) ?? 0) > 0;
  const focusInside = () => popover.contains(doc.activeElement);

  function setStatus(text, { canRetry = false } = {}) {
    status.textContent = text;
    retry.hidden = !canRetry;
  }

  function renderHead() {
    if (view === "days") {
      const before = addMonths(shown.year, shown.monthIndex, -1);
      const after = addMonths(shown.year, shown.monthIndex, 1);
      prev.disabled = !inRange(before.year);
      next.disabled = !inRange(after.year);
      prev.setAttribute("aria-label", "前の月");
      next.setAttribute("aria-label", "次の月");
      title.textContent = `${shown.year} ▼ ${shown.monthIndex + 1}月`;
      title.setAttribute("aria-label", `${shown.year}年${shown.monthIndex + 1}月、年と月を選ぶ`);
      title.setAttribute("aria-expanded", "false");
    } else {
      prev.disabled = !inRange(pickerYear - 1);
      next.disabled = !inRange(pickerYear + 1);
      prev.setAttribute("aria-label", "前の年");
      next.setAttribute("aria-label", "次の年");
      title.textContent = `${pickerYear} ▲`;
      title.setAttribute("aria-label", `${pickerYear}年、月の一覧を閉じる`);
      title.setAttribute("aria-expanded", "true");
    }
    prev.title = prev.getAttribute("aria-label");
    next.title = next.getAttribute("aria-label");
  }

  /** The day that takes the tab stop: the one moved to, the chosen one, today, or the first with visits. */
  function rovingDay() {
    const days = daysInMonth(shown.year, shown.monthIndex);
    const selected = getSelected();
    const today = localToday(now());
    const candidates = [
      focusDay,
      selected && selected.year === shown.year && selected.monthIndex === shown.monthIndex ? selected.day : null,
      today.year === shown.year && today.monthIndex === shown.monthIndex ? today.day : null,
    ];
    for (const day of candidates) if (day !== null && day <= days && selectable(day)) return day;
    for (let day = 1; day <= days; day++) if (selectable(day)) return day;
    return null;
  }

  function dayButtons() {
    return [...tbody.children].flatMap((row) => [...row.children].flatMap((cell) => [...cell.children]));
  }

  function renderGrid() {
    const focusedDay = dayButtons().includes(doc.activeElement) ? Number(doc.activeElement.dataset.day) : null;
    const selected = getSelected();
    const today = localToday(now());
    const roving = rovingDay();
    const loading = countOf(1) === null;
    grid.setAttribute("aria-busy", String(loading && open));
    grid.setAttribute("aria-label", `${shown.year}年${shown.monthIndex + 1}月`);
    let focusTarget = null;
    const rows = calendarWeeks(shown.year, shown.monthIndex).map((week) => {
      const row = make("tr");
      for (const [column, day] of week.entries()) {
        const cell = make("td");
        row.append(cell);
        if (day === null) continue;
        const count = countOf(day);
        const isToday = today.year === shown.year && today.monthIndex === shown.monthIndex && today.day === day;
        const isSelected = Boolean(selected && selected.year === shown.year && selected.monthIndex === shown.monthIndex && selected.day === day);
        const classes = ["calendar-day"];
        if (column === 0) classes.push("is-sun");
        if (column === 6) classes.push("is-sat");
        if (isToday) classes.push("is-today");
        if (isSelected) classes.push("is-selected");
        if (count > 0) classes.push("has-visits");
        const dayButton = button(classes.join(" "));
        dayButton.dataset.day = String(day);
        const number = make("span", "calendar-day-number", String(day));
        dayButton.append(number);
        if (count > 0) {
          const dot = make("span", "calendar-dot");
          dot.setAttribute("aria-hidden", "true");
          dayButton.append(dot);
        }
        dayButton.disabled = !(count > 0);
        dayButton.tabIndex = day === roving ? 0 : -1;
        dayButton.setAttribute("aria-label", dayLabel(shown.year, shown.monthIndex, day, count, { today: isToday, selected: isSelected }));
        if (isToday) dayButton.setAttribute("aria-current", "date");
        if (isSelected) cell.setAttribute("aria-selected", "true");
        cell.append(dayButton);
        if (day === focusedDay && !dayButton.disabled) focusTarget = dayButton;
      }
      return row;
    });
    tbody.replaceChildren(...rows);
    // Re-rendering keeps the keyboard where it was (or on the tab stop when that day is gone).
    if (focusedDay !== null) (focusTarget ?? dayButtons().find((candidate) => candidate.tabIndex === 0) ?? popover).focus();
  }

  function renderMonths() {
    const shownHere = pickerYear === shown.year;
    for (const [monthIndex, choice] of monthButtons.entries()) {
      choice.setAttribute("aria-label", `${pickerYear}年${monthIndex + 1}月`);
      if (shownHere && monthIndex === shown.monthIndex) choice.setAttribute("aria-current", "true");
      else choice.removeAttribute("aria-current");
      choice.disabled = !inRange(pickerYear);
    }
  }

  function render() {
    renderHead();
    grid.hidden = view !== "days";
    months.hidden = view !== "months";
    if (view === "days") renderGrid();
    else renderMonths();
  }

  /** Asks for the shown month's counts; only the latest request of the open popover may answer. */
  async function load({ keep = false } = {}) {
    const ticket = ++generation;
    const { year, monthIndex } = shown;
    if (!keep) counts = null;
    setStatus("閲覧日を読み込んでいます…");
    renderGrid();
    if (getVaultToken() === null) {
      setStatus("Vaultを確認できないため、閲覧日を表示できません。");
      return;
    }
    try {
      const answer = await visitDays(monthBounds(year, monthIndex), { send });
      if (ticket !== generation || !open) return;
      const current = getVaultToken();
      if (current === null || answer.vaultToken !== current) {
        // Counted in another vault than the list's (switched elsewhere): never shown as this one's.
        counts = null;
        renderGrid();
        setStatus("Vaultが切り替えられたため、閲覧日を表示できません。");
        if (current !== null) onVaultMismatch();
        return;
      }
      counts = { year, monthIndex, vaultToken: answer.vaultToken, values: answer.counts };
      const days = answer.counts.filter((count) => count > 0).length;
      setStatus(days === 0 ? "この月の閲覧記録はありません。" : `閲覧記録のある日: ${days}日`);
      renderGrid();
      if (doc.activeElement === popover && view === "days") dayButtons().find((candidate) => candidate.tabIndex === 0)?.focus();
    } catch (error) {
      if (ticket !== generation || !open) return;
      counts = null;
      renderGrid();
      setStatus(error instanceof SearchError ? error.message : "閲覧日を読み込めませんでした。", { canRetry: true });
      if (error instanceof SearchError && VAULT_UNAVAILABLE_CODES.has(error.code)) onVaultUnavailable();
    }
  }

  function showMonth(year, monthIndex) {
    shown = { year, monthIndex };
    focusDay = null;
    view = "days";
    render();
    void load();
  }

  function openCalendar() {
    if (open || launcher.disabled) return;
    open = true;
    view = "days";
    const selected = getSelected();
    shown = selected ? { year: selected.year, monthIndex: selected.monthIndex } : localToday(now());
    focusDay = selected ? selected.day : null;
    counts = null;
    popover.hidden = false;
    launcher.setAttribute("aria-expanded", "true");
    render();
    // Never past the bottom of the panel, however tall the toolbar above it is (it scrolls instead).
    const top = popover.getBoundingClientRect?.().top;
    if (Number.isFinite(top) && Number.isFinite(globalThis.innerHeight)) popover.style.maxHeight = `${Math.max(160, globalThis.innerHeight - top - 8)}px`;
    popover.focus();
    void load();
  }

  function close({ restoreFocus }) {
    if (!open) return;
    open = false;
    generation += 1; // an answer still on its way belongs to a closed popover
    counts = null;
    view = "days";
    popover.hidden = true;
    launcher.setAttribute("aria-expanded", "false");
    if (restoreFocus) (launcher.disabled ? fallbackFocus() : launcher.focus());
  }

  function choose(day) {
    const vaultToken = getVaultToken();
    if (!selectable(day) || counts.vaultToken !== vaultToken) return;
    const { year, monthIndex } = shown;
    close({ restoreFocus: true });
    onSelect({ year, monthIndex, day, vaultToken });
  }

  launcher.addEventListener("click", () => (open ? close({ restoreFocus: true }) : openCalendar()));
  prev.addEventListener("click", () => {
    if (view === "months") {
      pickerYear -= 1;
      render();
    } else {
      const month = addMonths(shown.year, shown.monthIndex, -1);
      showMonth(month.year, month.monthIndex);
    }
  });
  next.addEventListener("click", () => {
    if (view === "months") {
      pickerYear += 1;
      render();
    } else {
      const month = addMonths(shown.year, shown.monthIndex, 1);
      showMonth(month.year, month.monthIndex);
    }
  });
  title.addEventListener("click", () => {
    if (view === "days") {
      view = "months";
      pickerYear = shown.year;
      render();
      (monthButtons[shown.monthIndex] ?? title).focus();
    } else {
      // Back to the month as it was: choosing a year alone changes nothing.
      view = "days";
      render();
      title.focus();
    }
  });
  for (const [monthIndex, choice] of monthButtons.entries()) {
    choice.addEventListener("click", () => {
      if (choice.disabled) return;
      showMonth(pickerYear, monthIndex);
      title.focus();
    });
  }
  retry.addEventListener("click", () => {
    void load();
    title.focus();
  });
  grid.addEventListener("click", (event) => {
    const target = event.target.closest?.("button.calendar-day");
    if (target && tbody.contains(target) && !target.disabled) choose(Number(target.dataset.day));
  });
  grid.addEventListener("keydown", (event) => {
    const target = event.target.closest?.("button.calendar-day");
    if (!target || !tbody.contains(target)) return;
    const day = moveDay(shown.year, shown.monthIndex, Number(target.dataset.day), event.key, selectable);
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    if (day === null) return;
    focusDay = day;
    for (const candidate of dayButtons()) candidate.tabIndex = Number(candidate.dataset.day) === day ? 0 : -1;
    dayButtons().find((candidate) => Number(candidate.dataset.day) === day)?.focus();
  });
  // A day focused another way (a pointer) takes the tab stop, so there is always exactly one.
  grid.addEventListener("focusin", (event) => {
    const target = event.target.closest?.("button.calendar-day");
    if (!target || !tbody.contains(target)) return;
    focusDay = Number(target.dataset.day);
    for (const candidate of dayButtons()) candidate.tabIndex = candidate === target ? 0 : -1;
  });
  popover.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    close({ restoreFocus: true });
  });
  // Focus that leaves for another control closes it; nothing stays open behind the user's back.
  popover.addEventListener("focusout", (event) => {
    const to = event.relatedTarget;
    if (open && to && !popover.contains(to) && !launcher.contains(to)) close({ restoreFocus: false });
  });
  // A click elsewhere closes it without moving focus (the launcher toggles it itself).
  doc.addEventListener("pointerdown", (event) => {
    if (open && !popover.contains(event.target) && !launcher.contains(event.target)) close({ restoreFocus: false });
  }, true);

  return {
    get isOpen() {
      return open;
    },
    close,
    /** Whether the calendar can be used now (a host that offers it, and a list from a known vault). */
    setAvailable(available, reason = "") {
      launcher.disabled = !available;
      launcher.title = available ? "閲覧日で絞り込み" : reason;
      if (!available) close({ restoreFocus: focusInside() });
    },
    /** The vault changed: nothing of the old one stays (counts, the open popover). */
    vaultChanged() {
      close({ restoreFocus: focusInside() });
      counts = null;
    },
    /** New visits may have been recorded: the open month is counted again (its days stay usable meanwhile). */
    refresh() {
      if (open) void load({ keep: true });
    },
  };
}
