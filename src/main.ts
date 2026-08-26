import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { enable as enableAutostart, disable as disableAutostart, isEnabled as isAutostartEnabled } from "@tauri-apps/plugin-autostart";

type ModelInfo = {
  provider: string;
  model: string;
  label: string;
  note: string;
  price_per_min_usd: number;
};

type ProviderSettings = {
  provider: string;
  model: string;
};

const PROVIDER_KEY_HINTS: Record<string, { placeholder: string; url: string; host: string }> = {
  groq: { placeholder: "gsk_...", url: "https://console.groq.com/keys", host: "console.groq.com" },
  openai: { placeholder: "sk-...", url: "https://platform.openai.com/api-keys", host: "platform.openai.com" },
};

let models: ModelInfo[] = [];
let settings: ProviderSettings = { provider: "groq", model: "" };

function setStatus(selector: string, ok: boolean, message: string) {
  const el = document.querySelector<HTMLElement>(selector);
  if (!el) return;
  el.classList.toggle("ok", ok);
  const msg = el.querySelector<HTMLElement>(".msg");
  if (msg) msg.textContent = message;
}

async function applyPlatformUi() {
  const platform = await invoke<string>("platform_name");
  if (platform === "linux") return;
  document.querySelector("#hotkey-linux")?.setAttribute("hidden", "");
  document.querySelector("#hotkey-native")?.removeAttribute("hidden");
}

async function loadToggleCommand() {
  const commandEl = document.querySelector("#toggle-command");
  if (!commandEl) return;
  commandEl.textContent = await invoke<string>("toggle_command_hint");
}

async function copyToggleCommand() {
  const commandEl = document.querySelector("#toggle-command");
  const button = document.querySelector<HTMLButtonElement>("#copy-command");
  if (!commandEl?.textContent || !button) return;
  await navigator.clipboard.writeText(commandEl.textContent);
  button.classList.add("copied");
  setTimeout(() => button.classList.remove("copied"), 1200);
}

async function refreshHotkeyStatus() {
  const confirmed = await invoke<boolean>("hotkey_confirmed");
  setStatus(
    "#status",
    confirmed,
    confirmed ? "atalho confirmado." : "aguardando o primeiro Super+T…",
  );
}

async function refreshApiKeyStatus() {
  const configured = await invoke<boolean>("api_key_configured", { provider: settings.provider });
  setStatus(
    "#api-key-status",
    configured,
    configured ? "chave configurada." : "nenhuma chave salva ainda.",
  );
}

function formatPrice(perMinUsd: number): string {
  return `US$ ${perMinUsd.toFixed(perMinUsd < 0.001 ? 5 : 3)}/min`;
}

function renderProviderTabs() {
  document.querySelectorAll<HTMLButtonElement>("#provider-tabs button").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.provider === settings.provider);
  });
}

function renderModelList() {
  const list = document.querySelector<HTMLDivElement>("#model-list");
  if (!list) return;
  list.innerHTML = "";
  models
    .filter((m) => m.provider === settings.provider)
    .forEach((m) => {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "model-row" + (m.model === settings.model ? " selected" : "");
      row.innerHTML = `
        <span>
          <span class="model-name">${m.label}</span>
          <div class="model-note">${m.note}</div>
        </span>
        <span class="model-price">${formatPrice(m.price_per_min_usd)}</span>
      `;
      row.addEventListener("click", () => selectModel(m.provider, m.model));
      list.appendChild(row);
    });
}

function renderKeyHints() {
  const hint = PROVIDER_KEY_HINTS[settings.provider];
  const input = document.querySelector<HTMLInputElement>("#api-key-input");
  const link = document.querySelector<HTMLAnchorElement>("#provider-link");
  if (input) input.placeholder = hint.placeholder;
  if (link) link.textContent = hint.host;
}

async function selectModel(provider: string, model: string) {
  settings = { provider, model };
  await invoke("save_provider_settings", { provider, model });
  renderModelList();
  renderKeyHints();
  await refreshApiKeyStatus();
}

async function switchProvider(provider: string) {
  if (provider === settings.provider) return;
  const firstModel = models.find((m) => m.provider === provider);
  if (!firstModel) return;
  settings = { provider, model: firstModel.model };
  await invoke("save_provider_settings", { provider, model: firstModel.model });
  renderProviderTabs();
  renderModelList();
  renderKeyHints();
  await refreshApiKeyStatus();
}

async function loadProviderSettings() {
  [models, settings] = await Promise.all([
    invoke<ModelInfo[]>("list_provider_models"),
    invoke<ProviderSettings>("get_provider_settings"),
  ]);
  renderProviderTabs();
  renderModelList();
  renderKeyHints();
  await refreshApiKeyStatus();
}

async function saveApiKey(event: SubmitEvent) {
  event.preventDefault();
  const input = document.querySelector<HTMLInputElement>("#api-key-input");
  if (!input || !input.value.trim()) return;
  await invoke("save_api_key", { provider: settings.provider, key: input.value.trim() });
  input.value = "";
  await refreshApiKeyStatus();
}

async function setupAutostartToggle() {
  const toggle = document.querySelector<HTMLButtonElement>("#autostart-toggle");
  if (!toggle) return;
  const sync = (on: boolean) => {
    toggle.classList.toggle("on", on);
    toggle.setAttribute("aria-checked", String(on));
  };
  sync(await isAutostartEnabled());
  toggle.addEventListener("click", async () => {
    const turningOn = !toggle.classList.contains("on");
    turningOn ? await enableAutostart() : await disableAutostart();
    sync(await isAutostartEnabled());
  });
}

window.addEventListener("DOMContentLoaded", () => {
  applyPlatformUi();
  loadToggleCommand();
  refreshHotkeyStatus();
  loadProviderSettings();
  setupAutostartToggle();

  document
    .querySelector("#open-settings")
    ?.addEventListener("click", () => invoke("open_keyboard_settings"));
  document
    .querySelector("#copy-command")
    ?.addEventListener("click", copyToggleCommand);
  document.querySelector("#provider-link")?.addEventListener("click", (event) => {
    event.preventDefault();
    openUrl(PROVIDER_KEY_HINTS[settings.provider].url);
  });
  document
    .querySelector<HTMLFormElement>("#api-key-form")
    ?.addEventListener("submit", saveApiKey);
  document.querySelectorAll<HTMLButtonElement>("#provider-tabs button").forEach((btn) => {
    btn.addEventListener("click", () => switchProvider(btn.dataset.provider!));
  });
});
