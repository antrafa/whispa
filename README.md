# Whispa

**Dite. Cole. Pronto.**

Whispa é um app de desktop que transcreve sua voz em texto em qualquer aplicativo, com um atalho de teclado. Sem trocar de janela, sem copiar e colar de outro lugar — aperta o atalho, fala, e o texto já está na área de transferência.

<p align="center">
  <img src="docs/screenshots/hud.png" alt="HUD de gravação do Whispa" width="260">
</p>

---

## Por que o Whispa

Ferramentas de ditado já existem — mas quase todas são Mac/Windows only, fecham o código, ou empurram você pra um único provedor de IA com preço fixo. O Whispa nasceu porque nenhuma delas funcionava direito no Ubuntu com GNOME.

- **Nativo no Linux** — feito e testado primeiro pra Ubuntu/GNOME, onde a maioria das alternativas simplesmente não roda. Windows e macOS têm suporte no código (atalho global nativo) e build automatizado via CI, mas ainda não foram testados em hardware real dessas plataformas.
- **Sem vendor lock-in** — escolha o provedor de transcrição (Groq ou OpenAI) e o modelo, com o preço por minuto de cada um visível na hora de decidir.
- **Sua chave, seus dados** — o app nunca vê seu áudio nem sua chave de API. Tudo vai direto do seu computador pro provedor que você escolheu.
- **Feedback visual de verdade** — um indicador flutuante mostra quando está gravando, transcrevendo, ou se algo deu errado, então você nunca cola texto velho por engano.
- **Leve** — construído com Tauri (Rust + WebView nativo), não Electron. Sem Chromium embutido consumindo sua RAM.

## Como funciona

1. Aperta o atalho configurado (`Super+T` por padrão).
2. Fala.
3. Aperta de novo pra parar.
4. O áudio é transcrito pelo provedor escolhido e o texto cai direto na área de transferência — `Ctrl+V` no Linux/Windows ou `⌘+V` no macOS.

<p align="center">
  <img src="docs/screenshots/engine-picker.png" alt="Seleção de provedor e modelo com preço por minuto" width="420">
</p>

## Escolha seu motor de transcrição

| Provedor | Modelo | Custo aproximado |
|---|---|---|
| Groq | Whisper Large v3 Turbo | US$ 0,0007/min — o mais barato |
| Groq | Whisper Large v3 | US$ 0,002/min — melhor qualidade |
| OpenAI | GPT-4o Mini Transcribe | US$ 0,003/min |
| OpenAI | Whisper-1 | US$ 0,006/min |
| OpenAI | GPT-4o Transcribe | US$ 0,006/min |

Preços cobrados diretamente pelo provedor, na sua própria conta — o Whispa não intermedia, não cobra por uso e não vê seu áudio. Você cria uma chave gratuita em [console.groq.com](https://console.groq.com) ou [platform.openai.com](https://platform.openai.com), cola na tela de configuração, e pronto.

## Plataformas

| SO | Status | Atalho global |
|---|---|---|
| Linux (Ubuntu/GNOME) | Testado e validado | Atalho personalizado guiado nas Configurações do sistema (necessário no Wayland) |
| Windows | Código pronto, build via CI, **não testado em máquina real** | Registrado automaticamente pelo app (`Alt+Shift+D`, ainda não verificado) |
| macOS | Código pronto, build via CI, **não testado em máquina real** | Registrado automaticamente pelo app (`⌥⇧D` / Option+Shift+D, ainda não verificado) |

## Instalação

Instaladores pra Linux, Windows e macOS são gerados automaticamente a cada release, na aba [Releases](https://github.com/antrafa/whispa/releases). O app do macOS recebe uma assinatura ad-hoc válida, evitando o falso aviso de arquivo corrompido, mas ainda não tem Developer ID nem notarização — o Gatekeeper pode pedir liberação manual em Privacidade e Segurança. No Windows, o SmartScreen também pode avisar que o app não é de um desenvolvedor reconhecido.

Ou construa a partir do código-fonte:

### Pré-requisitos (Ubuntu/Debian)

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libssl-dev libasound2-dev \
  build-essential curl wget file
```

Você também precisa do [Rust](https://rustup.rs) e do [Node.js](https://nodejs.org) (18+).

### Rodar em modo desenvolvimento

```bash
git clone git@github.com:antrafa/whispa.git
cd whispa
npm install
npm run tauri dev
```

Na primeira execução, o app abre uma tela guiada pra cadastrar o atalho de teclado no GNOME e configurar a chave de API do provedor escolhido.

### Gerar um build de produção

```bash
npm run tauri build
```

## Atalho de teclado

**Linux/GNOME:** o Wayland não deixa apps de terceiros capturarem atalhos globais sozinhos. O Whispa contorna isso registrando um atalho personalizado nas Configurações do sistema, que roda `whispa --toggle` — a própria tela de configuração te guia por esse passo com o comando já pronto pra colar.

**Windows/macOS:** essas plataformas suportam registro de atalho global nativo de verdade, então o app registra `Alt+Shift+D` sozinho na primeira execução, sem passo manual. Essa combinação foi escolhida pra evitar colisão com atalhos comuns de navegador, mas **ainda não foi verificada em hardware Windows/Mac real** — se conflitar com algo no seu sistema, abra uma issue.

## Atualização

A partir da v0.1.2, o app checa sozinho por versão nova ao abrir e mostra um aviso na tela de configuração pra instalar com um clique (baixa, instala e reinicia). Isso só funciona pra quem já está em v0.1.2+ — versões anteriores precisam de uma última atualização manual:

```bash
sudo dpkg -i Whispa_X.Y.Z_amd64.deb
```

O update automático só encontra release **publicada** (não draft) — verifique isso antes de esperar o aviso aparecer.

## Stack técnica

- **[Tauri 2](https://tauri.app)** — shell nativo (Rust) + WebView do sistema, sem Chromium embutido
- **Rust** — captura de áudio ([cpal](https://github.com/RustAudio/cpal)), integração com o sistema, chamadas HTTP
- **TypeScript + Vite** — as três janelas do app (configuração, HUD, sobre)
- Groq e OpenAI — `POST /v1/audio/transcriptions`, formato multipart compatível entre os dois

## Roadmap

- [x] Instalador empacotado (`.deb` / `.AppImage` / `.msi` / `.dmg`, via CI)
- [x] Atualização automática (a partir da v0.1.2)
- [ ] Validar Windows e macOS em hardware real (código pronto, não testado)
- [x] Assinatura ad-hoc no macOS (evita o falso aviso de arquivo corrompido)
- [ ] Developer ID + notarização no macOS e assinatura no Windows (remove os avisos de "app não confiável")
- [ ] Atalho global via XDG Desktop Portal no Linux (funcionaria em qualquer DE, não só GNOME)
- [ ] Chave de API guardada no keychain do sistema em vez de arquivo local

## Privacidade

O Whispa não tem servidor, não coleta analytics e não guarda o áudio depois de transcrito. A chave de API fica só na sua máquina (`~/.config/whispa/` no Linux, permissão restrita ao seu usuário). O único destino do seu áudio é o provedor de IA que você escolheu, usando a sua própria chave.

## Contribuindo

Issues e PRs são bem-vindos. Se for propor uma mudança grande, abre uma issue primeiro pra alinhar o design antes de codar.

## Sobre

<p align="center">
  <img src="docs/screenshots/about.png" alt="Tela Sobre do Whispa" width="320">
</p>

Desenvolvido por **Antonio Rafael Ortega** — [github.com/antrafa](https://github.com/antrafa)
