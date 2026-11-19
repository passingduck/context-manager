import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { initTransport } from "./api/transport";
import { App } from "./App";
import "./index.css";

initTransport();

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
