import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type HudState = {
  state: "idle" | "recording" | "processing" | "success" | "error";
  title: string;
  message: string;
  detail: string;
  can_retry: boolean;
  has_recovery: boolean;
  recovery_message: string;
  started_at_ms: number;
  recording_limit_secs: number;
};

const pill = document.querySelector<HTMLDivElement>("#pill")!;
const label = document.querySelector<HTMLSpanElement>("#label")!;
const message = document.querySelector<HTMLParagraphElement>("#message")!;
const detail = document.querySelector<HTMLParagraphElement>("#detail")!;
const timer = document.querySelector<HTMLSpanElement>("#timer")!;
const retry = document.querySelector<HTMLButtonElement>("#retry")!;
const dismiss = document.querySelector<HTMLButtonElement>("#dismiss")!;
const recovery = document.querySelector<HTMLParagraphElement>("#recovery")!;
const actions = document.querySelector<HTMLDivElement>("#actions")!;
let current: HudState | undefined;
let commandPending = false;
let revision = 0;

function clock(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function updateTimer() {
  if (!current) return;
  const elapsed = Math.max(0, Math.floor((Date.now() - current.started_at_ms) / 1000));
  timer.hidden = current.state !== "recording" && current.state !== "processing";
  timer.textContent = current.state === "recording"
    ? `${clock(Math.min(elapsed, current.recording_limit_secs))} / ${clock(current.recording_limit_secs)}`
    : `${clock(elapsed)} aguardando`;
}

function render(state: HudState) {
  current = state;
  pill.dataset.state = state.state;
  label.textContent = state.title;
  message.textContent = state.message;
  message.hidden = !state.message;
  detail.textContent = state.detail;
  detail.hidden = !state.detail;
  recovery.hidden = !state.has_recovery;
  recovery.textContent = state.recovery_message;
  actions.hidden = state.state !== "error";
  retry.hidden = !state.can_retry;
  retry.disabled = commandPending;
  dismiss.disabled = commandPending;
  dismiss.textContent = state.has_recovery ? "Descartar" : "Fechar";
  updateTimer();
}

const isTauri = typeof window !== "undefined" && Boolean((window as any).__TAURI_INTERNALS__);

async function runCommand(command: string) {
  if (commandPending) return;
  commandPending = true;
  retry.disabled = true;
  dismiss.disabled = true;
  try {
    if (isTauri) {
      await invoke(command);
    } else {
      if (command === "dismiss_transcription") {
        render({
          state: "idle",
          title: "",
          message: "",
          detail: "",
          can_retry: false,
          has_recovery: false,
          recovery_message: "",
          started_at_ms: Date.now(),
          recording_limit_secs: 180,
        });
      } else if (command === "retry_transcription") {
        render({
          state: "processing",
          title: "TRANSCREVENDO",
          message: "",
          detail: "Tentando novamente...",
          can_retry: false,
          has_recovery: true,
          recovery_message: "",
          started_at_ms: Date.now(),
          recording_limit_secs: 180,
        });
      }
    }
  } catch (error) {
    message.hidden = false;
    message.textContent = String(error);
  } finally {
    commandPending = false;
    retry.disabled = false;
    dismiss.disabled = false;
  }
}

retry.addEventListener("click", () => runCommand("retry_transcription"));
dismiss.addEventListener("click", () => runCommand("dismiss_transcription"));
setInterval(updateTimer, 1000);

async function initialize() {
  const urlParams = new URLSearchParams(window.location.search);
  const mockState = urlParams.get("state") as HudState["state"] | null;

  if (mockState) {
    render({
      state: mockState,
      title: urlParams.get("title") || (mockState === "error" ? "O provedor demorou demais" : mockState.toUpperCase()),
      message: urlParams.get("message") || (mockState === "error" ? "Não recebemos a transcrição a tempo. Confira sua conexão e tente novamente." : ""),
      detail: urlParams.get("detail") || "Groq · áudio 45s · 4.3 MB · espera 12s",
      can_retry: urlParams.get("can_retry") !== "false",
      has_recovery: urlParams.get("has_recovery") !== "false",
      recovery_message: urlParams.get("recovery_message") || "Áudio mantido em memória. Descartar ou fechar o app apaga esta gravação.",
      started_at_ms: Date.now() - 45000,
      recording_limit_secs: 180,
    });
    return;
  }

  if (isTauri) {
    await listen<HudState>("hud-state", ({ payload }) => {
      revision++;
      render(payload);
    });
    const requestedAt = revision;
    const snapshot = await invoke<HudState>("get_hud_state");
    if (requestedAt === revision) render(snapshot);
  }
}
initialize().catch((error) => console.error("Não foi possível carregar o indicador", error));
