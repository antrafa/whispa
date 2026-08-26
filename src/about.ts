import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";

const GITHUB_URL = "https://github.com/antrafa/whispa";

window.addEventListener("DOMContentLoaded", async () => {
  const versionEl = document.querySelector("#about-version");
  if (versionEl) versionEl.textContent = `v${await getVersion()}`;

  document
    .querySelector("#open-github")
    ?.addEventListener("click", () => openUrl(GITHUB_URL));
});
