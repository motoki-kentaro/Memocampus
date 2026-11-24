// Loaded before every test file (`node --import ./tests/extension/i18n-setup.js --test`, see
// package.json): extension modules read their text through chrome.i18n, so a test process starts with
// a chrome object that has the shared Japanese chrome.i18n stand-in. Tests that install their own
// chrome object pass `i18n: chromeI18n` along (see i18n-mock.js).
import { chromeI18n } from "./i18n-mock.js";

globalThis.chrome = { i18n: chromeI18n };
