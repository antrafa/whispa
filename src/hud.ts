import { listen } from "@tauri-apps/api/event";

type HudState = "recording" | "processing" | "success" | "error";

const LABELS: Record<HudState, string> = {
  recording: "GRAVANDO",
  processing: "TRANSCREVENDO",
  success: "COPIADO",
  error: "ERRO",
};

const pill = document.querySelector<HTMLDivElement>("#pill");
const label = document.querySelector<HTMLSpanElement>("#label");

listen<{ state: HudState }>("hud-state", (event) => {
  if (!pill || !label) return;
  const { state } = event.payload;
  pill.dataset.state = state;
  label.textContent = LABELS[state];
});
