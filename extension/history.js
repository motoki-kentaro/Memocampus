// The full-page History (Slice 2) in a Chrome tab: wires the controller to the real page and Chrome.
import { chromeNativeTransport } from "./search-client.js";
import { createHistoryApp } from "./history-app.js";

const app = createHistoryApp({
  document,
  win: window,
  send: chromeNativeTransport(chrome.runtime),
  tabs: chrome.tabs,
  openSettings: () => void chrome.runtime.openOptionsPage().catch(() => {}),
});
void app.start();
