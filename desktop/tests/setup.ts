import { afterEach, mock } from "bun:test";
import { clearMocks } from "@tauri-apps/api/mocks";
import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost/",
});
for (const name of [
  "window",
  "document",
  "navigator",
  "HTMLElement",
  "HTMLDialogElement",
  "Node",
  "Event",
  "MouseEvent",
  "MutationObserver",
] as const) {
  const value = name === "window" ? dom.window : dom.window[name];
  Object.defineProperty(globalThis, name, { configurable: true, value });
}
Object.defineProperty(globalThis, "getComputedStyle", {
  configurable: true,
  value: dom.window.getComputedStyle.bind(dom.window),
});
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
  configurable: true,
  writable: true,
  value: true,
});
// jsdom does not implement native modal dialogs.
HTMLDialogElement.prototype.showModal = function () {
  this.open = true;
};
HTMLDialogElement.prototype.close = function (value) {
  if (value !== undefined) this.returnValue = value;
  this.open = false;
  this.dispatchEvent(new Event("close"));
};

const { cleanup } = await import("@testing-library/react");
afterEach(() => {
  cleanup();
  clearMocks();
  mock.restore();
});
