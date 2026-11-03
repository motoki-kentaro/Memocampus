import { chromeNativeTransport } from "./search-client.js";
import { startOptionsPage } from "./options-page.js";

startOptionsPage({ document, window, send: chromeNativeTransport(chrome.runtime) });
