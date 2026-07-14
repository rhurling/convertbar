import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// Suppress the webview's default context menu (Reload etc.) in production;
// dev builds keep it for debugging. Editable elements keep the native menu
// so copy/paste still works in the search field and settings inputs.
if (!import.meta.env.DEV) {
  document.addEventListener("contextmenu", (e) => {
    const target = e.target as HTMLElement;
    if (!target.closest("input, textarea, [contenteditable]")) {
      e.preventDefault();
    }
  });
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
