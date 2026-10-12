import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { setNonce } from "get-nonce";
import { App } from "./App";
import { initializeLanguage } from "./i18n";
import "./style.css";

// Tauri replaces the bundled style anchor's nonce for each webview response.
const styleNonce =
  document.querySelector<HTMLStyleElement>("#xrun-style-nonce")?.nonce;
if (styleNonce) setNonce(styleNonce);

async function start() {
  await initializeLanguage();
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void start();
