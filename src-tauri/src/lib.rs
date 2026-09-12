mod transcription;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{image::Image, AppHandle, Emitter, Manager, PhysicalPosition};
use transcription::{ErrorKind, Failure};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_clipboard_manager::ClipboardExt;

const SETUP_WINDOW_LABEL: &str = "setup";
const HUD_WINDOW_LABEL: &str = "hud";
const ABOUT_WINDOW_LABEL: &str = "about";
const MAX_RECORDING_DURATION: std::time::Duration = std::time::Duration::from_secs(180);
const IDLE_ICON: &[u8] = include_bytes!("../icons/icon.png");
const RECORDING_ICON: &[u8] = include_bytes!("../icons/tray-recording.png");

#[cfg(target_os = "linux")]
const HOTKEY_DISPLAY_NAME: &str = "Super+T";
#[cfg(target_os = "macos")]
const HOTKEY_DISPLAY_NAME: &str = "⌥+⇧+D";
#[cfg(target_os = "windows")]
const HOTKEY_DISPLAY_NAME: &str = "Alt+Shift+D";

struct AudioFormat {
    sample_rate: u32,
    channels: u16,
}

struct RecordingHandle {
    stop_flag: Arc<AtomicBool>,
}

struct AppState {
    recording: Mutex<Option<RecordingHandle>>,
    setup_done: AtomicBool,
    cycle_id: AtomicU64,
    busy: AtomicBool,
    recovery: Mutex<Option<Recovery>>,
    hud: Mutex<HudState>,
}

impl AppState {
    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            recording: Mutex::new(None),
            setup_done: AtomicBool::new(false),
            cycle_id: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            recovery: Mutex::new(None),
            hud: Mutex::new(HudState::new("idle", "")),
        }
    }

    fn can_start_new_recording(&self) -> bool {
        if self.recording.lock().unwrap().is_some() {
            return false;
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        if self.recovery.lock().unwrap().is_some() {
            self.busy.store(false, Ordering::SeqCst);
            return false;
        }
        true
    }

    fn prepare_retry(&self) -> Result<(u64, Recovery), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("Já existe um ditado em andamento.".into());
        }
        if !self.hud.lock().unwrap().can_retry {
            self.busy.store(false, Ordering::SeqCst);
            return Err("Esta falha exige uma nova gravação.".into());
        }
        let recovery = self.recovery.lock().unwrap().clone();
        let Some(recovery) = recovery else {
            self.busy.store(false, Ordering::SeqCst);
            return Err("Não há gravação para tentar novamente.".into());
        };
        let cycle_id = self.cycle_id.fetch_add(1, Ordering::SeqCst) + 1;
        Ok((cycle_id, recovery))
    }

    fn perform_dismiss(&self) -> Result<(), String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("Aguarde o ditado terminar.".into());
        }
        self.recovery.lock().unwrap().take();
        self.cycle_id.fetch_add(1, Ordering::SeqCst);
        *self.hud.lock().unwrap() = HudState::new("idle", "");
        self.busy.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn build_failure_hud(&self, error: &Failure, detail: &str) -> HudState {
        let (has_recovery, recovery_message) = match self.recovery.lock().unwrap().as_ref() {
            Some(Recovery::Audio(_)) => (true, "Áudio mantido em memória. Descartar ou fechar o app apaga esta gravação."),
            Some(Recovery::Text(_)) => (true, "Texto mantido em memória. Descartar ou fechar o app apaga esta transcrição."),
            None => (false, ""),
        };
        let mut hud = HudState::new("error", &error.title);
        hud.message = error.message.clone();
        hud.detail = detail.to_owned();
        hud.can_retry = has_recovery && error.retryable;
        hud.has_recovery = has_recovery;
        hud.recovery_message = recovery_message.into();
        *self.hud.lock().unwrap() = hud.clone();
        self.busy.store(false, Ordering::SeqCst);
        hud
    }
}

#[derive(Clone)]
struct PendingAudio {
    bytes: Arc<Vec<u8>>,
    duration_seconds: f64,
    limit_reached: bool,
}

#[derive(Clone)]
enum Recovery {
    Audio(PendingAudio),
    Text(String),
}

#[derive(Clone, serde::Serialize)]
struct HudState {
    state: String,
    title: String,
    message: String,
    detail: String,
    can_retry: bool,
    has_recovery: bool,
    recovery_message: String,
    started_at_ms: u64,
    recording_limit_secs: u64,
}

impl HudState {
    fn new(state: &str, title: &str) -> Self {
        Self {
            state: state.into(), title: title.into(), message: String::new(),
            detail: String::new(), can_retry: false, has_recovery: false, recovery_message: String::new(),
            started_at_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis() as u64,
            recording_limit_secs: MAX_RECORDING_DURATION.as_secs(),
        }
    }
}

#[derive(serde::Serialize, Clone, Copy)]
struct ModelInfo {
    provider: &'static str,
    model: &'static str,
    label: &'static str,
    note: &'static str,
    price_per_min_usd: f64,
}

// Preços levantados manualmente (ago/2026); podem ficar desatualizados —
// não há API pública de pricing pra consultar em tempo real.
const MODELS: &[ModelInfo] = &[
    ModelInfo {
        provider: "groq",
        model: "whisper-large-v3-turbo",
        label: "Whisper Large v3 Turbo",
        note: "rápido e o mais barato",
        price_per_min_usd: 0.00067,
    },
    ModelInfo {
        provider: "groq",
        model: "whisper-large-v3",
        label: "Whisper Large v3",
        note: "melhor qualidade da Groq",
        price_per_min_usd: 0.00185,
    },
    ModelInfo {
        provider: "openai",
        model: "gpt-4o-mini-transcribe",
        label: "GPT-4o Mini Transcribe",
        note: "mais barato da OpenAI",
        price_per_min_usd: 0.003,
    },
    ModelInfo {
        provider: "openai",
        model: "whisper-1",
        label: "Whisper-1",
        note: "clássico da OpenAI",
        price_per_min_usd: 0.006,
    },
    ModelInfo {
        provider: "openai",
        model: "gpt-4o-transcribe",
        label: "GPT-4o Transcribe",
        note: "modelo mais novo da OpenAI",
        price_per_min_usd: 0.006,
    },
];

fn provider_base_url(provider: &str) -> &'static str {
    match provider {
        "openai" => "https://api.openai.com/v1/audio/transcriptions",
        _ => "https://api.groq.com/openai/v1/audio/transcriptions",
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ProviderSettings {
    provider: String,
    model: String,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            provider: "groq".to_string(),
            model: "whisper-large-v3-turbo".to_string(),
        }
    }
}

fn config_dir(app: &AppHandle) -> std::path::PathBuf {
    let dir = app
        .path()
        .config_dir()
        .expect("diretorio de config do sistema")
        .join("whispa");
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn setup_marker_path(app: &AppHandle) -> std::path::PathBuf {
    config_dir(app).join("setup-done")
}

fn settings_path(app: &AppHandle) -> std::path::PathBuf {
    config_dir(app).join("settings.json")
}

fn read_settings(app: &AppHandle) -> ProviderSettings {
    std::fs::read_to_string(settings_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_settings(app: &AppHandle, settings: &ProviderSettings) -> Result<(), String> {
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(settings_path(app), json).map_err(|e| e.to_string())
}

fn api_key_path(app: &AppHandle, provider: &str) -> std::path::PathBuf {
    config_dir(app).join(format!("{provider}-api-key"))
}

fn read_api_key(app: &AppHandle, provider: &str) -> Option<String> {
    std::fs::read_to_string(api_key_path(app, provider))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn has_api_key(app: &AppHandle, provider: &str) -> bool {
    read_api_key(app, provider).is_some()
}

fn has_active_api_key(app: &AppHandle) -> bool {
    has_api_key(app, &read_settings(app).provider)
}

fn is_toggle_request(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "--toggle")
}

// GTK/X11 no Linux não é thread-safe: toda chamada que toca janela ou tray
// precisa rodar na main thread, mesmo quando disparada de uma thread de
// gravação/transcrição, senão o processo derruba (xcb assertion).
fn set_tray_state(app: &AppHandle, recording: bool) {
    let main_thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let app = main_thread_app;
        let Some(tray) = app.tray_by_id("main") else {
            return;
        };
        let bytes = if recording { RECORDING_ICON } else { IDLE_ICON };
        if let Ok(icon) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(icon));
        }
        let tooltip = if recording {
            format!("whispa — gravando ({HOTKEY_DISPLAY_NAME} para parar)")
        } else {
            format!("whispa — {HOTKEY_DISPLAY_NAME} para ditar")
        };
        let _ = tray.set_tooltip(Some(&tooltip));
    });
}

fn mark_setup_done(app: &AppHandle, state: &AppState) {
    if !state.setup_done.swap(true, Ordering::SeqCst) {
        std::fs::write(setup_marker_path(app), b"1").ok();
    }
    maybe_hide_setup_window(app, state);
}

fn maybe_hide_setup_window(app: &AppHandle, state: &AppState) {
    if !state.setup_done.load(Ordering::SeqCst) || !has_active_api_key(app) {
        return;
    }
    let main_thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = main_thread_app.get_webview_window(SETUP_WINDOW_LABEL) {
            let _ = window.hide();
        }
    });
}

fn position_hud(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let margin_bottom = (64.0 * monitor.scale_factor()) as i32;
    let x = monitor.position().x + (monitor.size().width as i32 - size.width as i32) / 2;
    let y = monitor.position().y + monitor.size().height as i32 - size.height as i32 - margin_bottom;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

fn present_hud(app: &AppHandle, payload: HudState) {
    let cycle_id = app.state::<AppState>().cycle_id.load(Ordering::SeqCst);
    *app.state::<AppState>().hud.lock().unwrap() = payload.clone();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if handle.state::<AppState>().cycle_id.load(Ordering::SeqCst) != cycle_id { return; }
        let Some(window) = handle.get_webview_window(HUD_WINDOW_LABEL) else { return; };
        let (width, height) = if payload.state == "error" { (460.0, 380.0) } else if !payload.message.is_empty() { (340.0, 180.0) } else { (340.0, 110.0) };
        let _ = window.set_size(tauri::LogicalSize::new(width, height));
        position_hud(&window);
        let _ = handle.emit("hud-state", &payload);
        let _ = window.show();
    });
}

fn show_hud(app: &AppHandle, state: &str) {
    let title = match state {
        "recording" => "GRAVANDO",
        "processing" => "TRANSCREVENDO",
        "success" => "COPIADO",
        _ => "ERRO",
    };
    present_hud(app, HudState::new(state, title));
}

fn hide_hud_after(app: &AppHandle, expected_cycle_id: u64, delay: std::time::Duration) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let state = handle.state::<AppState>();
            if state.cycle_id.load(Ordering::SeqCst) != expected_cycle_id { return; }
            if state.hud.lock().unwrap().state != "success" { return; }
            if let Some(window) = handle.get_webview_window(HUD_WINDOW_LABEL) {
                let _ = window.hide();
            }
        });
    });
}

fn fail_cycle(app: &AppHandle, error: Failure, detail: String) {
    let state = app.state::<AppState>();
    let hud = state.build_failure_hud(&error, &detail);
    // Diagnostics contain no audio, transcription, API key, or raw provider response.
    if let Ok(json) = serde_json::to_vec_pretty(&serde_json::json!({ "error": error, "context": hud })) {
        let _ = std::fs::write(config_dir(app).join("last-error.json"), json);
    }
    set_tray_state(app, false);
    present_hud(app, hud);
}

fn deliver_text(app: &AppHandle, cycle_id: u64, text: String) {
    let state = app.state::<AppState>();
    *state.recovery.lock().unwrap() = Some(Recovery::Text(text.clone()));
    match write_to_clipboard(app, &text) {
        Ok(()) => {
            state.recovery.lock().unwrap().take();
            set_tray_state(app, false);
            show_hud(app, "success");
            state.busy.store(false, Ordering::SeqCst);
            hide_hud_after(app, cycle_id, std::time::Duration::from_millis(1800));
        }
        Err(_) => fail_cycle(app, Failure::new(ErrorKind::Clipboard, "Não foi possível copiar", "A transcrição está pronta. Tente novamente para copiar o texto, sem reenviar o áudio.", true), String::new()),
    }
}

#[tauri::command]
fn get_hud_state(app: AppHandle) -> HudState {
    app.state::<AppState>().hud.lock().unwrap().clone()
}

#[tauri::command]
fn retry_transcription(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (cycle_id, recovery) = state.prepare_retry()?;
    show_hud(&app, "processing");
    std::thread::spawn(move || match recovery {
        Recovery::Audio(audio) => transcribe_and_deliver(&app, cycle_id, audio),
        Recovery::Text(text) => deliver_text(&app, cycle_id, text),
    });
    Ok(())
}

#[tauri::command]
fn dismiss_transcription(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    state.perform_dismiss()?;
    if let Some(window) = app.get_webview_window(HUD_WINDOW_LABEL) { let _ = window.hide(); }
    Ok(())
}

fn start_recording(app: &AppHandle, state: &AppState) {
    mark_setup_done(app, state);

    let cycle_id = state.cycle_id.fetch_add(1, Ordering::SeqCst) + 1;
    let stop_flag = Arc::new(AtomicBool::new(false));
    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let format: Arc<Mutex<Option<AudioFormat>>> = Arc::new(Mutex::new(None));

    let thread_stop_flag = stop_flag.clone();
    let thread_samples = samples.clone();
    let thread_format = format.clone();
    let thread_app = app.clone();

    *state.recording.lock().unwrap() = Some(RecordingHandle { stop_flag });
    set_tray_state(app, true);
    show_hud(app, "recording");

    std::thread::spawn(move || {
        let limit_reached = record_until_stopped(&thread_samples, &thread_format, &thread_stop_flag);
        // Cobre tanto o corte por MAX_RECORDING quanto qualquer saida
        // antecipada (erro de microfone): sem isso, o proximo toggle
        // interpretaria o app como "ainda gravando" pra sempre.
        thread_app
            .state::<AppState>()
            .recording
            .lock()
            .unwrap()
            .take();
        finish_recording(&thread_app, cycle_id, &thread_samples, &thread_format, limit_reached);
    });

}

fn toggle_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    let recording = state.recording.lock().unwrap();
    if let Some(handle) = recording.as_ref() {
        if !handle.stop_flag.swap(true, Ordering::SeqCst) {
            show_hud(app, "processing");
        }
        return;
    }
    drop(recording);
    if !state.can_start_new_recording() {
        if state.recovery.lock().unwrap().is_some() {
            let hud = state.hud.lock().unwrap().clone();
            present_hud(app, hud);
        }
        return;
    }
    start_recording(app, &state);
}

fn record_until_stopped(
    samples: &Arc<Mutex<Vec<f32>>>,
    format_out: &Arc<Mutex<Option<AudioFormat>>>,
    stop_flag: &Arc<AtomicBool>,
) -> bool {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        eprintln!("whispa: nenhum microfone encontrado");
        return false;
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("whispa: falha ao ler configuracao do microfone: {e}");
            return false;
        }
    };

    *format_out.lock().unwrap() = Some(AudioFormat {
        sample_rate: config.sample_rate().0,
        channels: config.channels(),
    });

    let err_fn = |err| eprintln!("whispa: erro no stream de audio: {err}");
    let stream_samples = samples.clone();
    let sample_format = config.sample_format();

    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| stream_samples.lock().unwrap().extend_from_slice(data),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                let mut buf = stream_samples.lock().unwrap();
                buf.extend(data.iter().map(|s| *s as f32 / i16::MAX as f32));
            },
            err_fn,
            None,
        ),
        other => {
            eprintln!("whispa: formato de audio nao suportado: {other:?}");
            return false;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            eprintln!("whispa: falha ao abrir stream de audio: {e}");
            return false;
        }
    };

    if let Err(e) = stream.play() {
        eprintln!("whispa: falha ao iniciar gravacao: {e}");
        return false;
    }

    let deadline = std::time::Instant::now() + MAX_RECORDING_DURATION;
    while !stop_flag.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= deadline {
            eprintln!("whispa: gravacao cortada em {MAX_RECORDING_DURATION:?} (limite de seguranca)");
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // stream é dropado aqui, o que encerra a captura
    false
}

fn finish_recording(
    app: &AppHandle,
    cycle_id: u64,
    samples: &Arc<Mutex<Vec<f32>>>,
    format: &Arc<Mutex<Option<AudioFormat>>>,
    limit_reached: bool,
) {
    let mut hud = HudState::new("processing", "TRANSCREVENDO");
    if limit_reached { hud.message = "Limite de 3 minutos atingido. Transcrevendo o áudio capturado.".into(); }
    present_hud(app, hud);
    let result = {
        let samples = samples.lock().unwrap();
        let format = format.lock().unwrap();
        match format.as_ref() {
            Some(format) => transcription::encode_audio(&samples, format.sample_rate, format.channels)
                .map(|bytes| PendingAudio {
                    bytes: Arc::new(bytes),
                    duration_seconds: samples.len() as f64 / format.channels as f64 / format.sample_rate as f64,
                    limit_reached,
                }),
            None => Err(Failure::new(ErrorKind::Audio, "Microfone indisponível", "Confira o microfone selecionado e permita a gravação nos ajustes do sistema.", false)),
        }
    };
    // Release the large raw recording before the network request.
    *samples.lock().unwrap() = Vec::new();
    match result {
        Ok(audio) => {
            *app.state::<AppState>().recovery.lock().unwrap() = Some(Recovery::Audio(audio.clone()));
            transcribe_and_deliver(app, cycle_id, audio);
        }
        Err(error) => fail_cycle(app, error, String::new()),
    }
}

fn transcribe_and_deliver(app: &AppHandle, cycle_id: u64, audio: PendingAudio) {
    let settings = read_settings(app);
    let provider = if settings.provider == "openai" { "OpenAI" } else { "Groq" };
    let detail = format!("{provider} · áudio {:.0}s · {:.1} MB{}", audio.duration_seconds,
        audio.bytes.len() as f64 / 1_000_000.0, if audio.limit_reached { " · limite de 3 min" } else { "" });
    let Some(api_key) = read_api_key(app, &settings.provider) else {
        fail_cycle(app, Failure::new(ErrorKind::Credentials, "Configure sua chave de API", "Salve a chave do provedor nas Configurações e tente novamente com este áudio.", true), detail);
        return;
    };
    let started = std::time::Instant::now();
    let result = tauri::async_runtime::block_on(transcription::transcribe(
        provider_base_url(&settings.provider), &api_key, &settings.model, &audio.bytes));
    match result {
        Ok(text) => deliver_text(app, cycle_id, text),
        Err(error) => fail_cycle(app, error, format!("{detail} · espera {:.0}s", started.elapsed().as_secs_f64())),
    }
}

#[cfg(target_os = "macos")]
fn write_to_macos_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // O clipboard-manager/arboard pode retornar Ok no macOS sem atualizar o
    // NSPasteboard. pbcopy usa a integração nativa do sistema; o pbpaste logo
    // depois impede que um falso sucesso chegue ao HUD.
    let mut child = Command::new("/usr/bin/pbcopy")
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("falha ao iniciar pbcopy: {error}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "pbcopy iniciou sem stdin".to_string())?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|error| format!("falha ao enviar texto ao pbcopy: {error}"))?;
    drop(stdin);

    let status = child
        .wait()
        .map_err(|error| format!("falha ao aguardar pbcopy: {error}"))?;
    if !status.success() {
        return Err(format!("pbcopy terminou com status {status}"));
    }

    let pasted = Command::new("/usr/bin/pbpaste")
        .env("LANG", "en_US.UTF-8")
        .output()
        .map_err(|error| format!("falha ao confirmar clipboard com pbpaste: {error}"))?;
    if !pasted.status.success() {
        return Err(format!("pbpaste terminou com status {}", pasted.status));
    }
    if pasted.stdout != text.as_bytes() {
        return Err("clipboard divergiu do texto transcrito".to_string());
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn write_to_clipboard(_app: &AppHandle, text: &str) -> Result<(), String> {
    write_to_macos_clipboard(text)
}

#[cfg(not(target_os = "macos"))]
fn write_to_clipboard(app: &AppHandle, text: &str) -> Result<(), String> {
    app.clipboard()
        .write_text(text.to_string())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn transcription_accepts_a_response_after_eight_seconds() {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(15))).unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" { break; }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(9));
            let body = r#"{"text":"descrição completa"}"#;
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            socket.write_all(response.as_bytes()).ok();
        });
        let result = tauri::async_runtime::block_on(super::transcription::transcribe(&url, "test-key", "test-model", b"test audio"));
        server.join().unwrap();
        assert_eq!(result.unwrap(), "descrição completa");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_clipboard_write_is_observable() {
        let previous = std::process::Command::new("/usr/bin/pbpaste")
            .env("LANG", "en_US.UTF-8")
            .output()
            .expect("ler clipboard antes do teste");
        let previous = String::from_utf8_lossy(&previous.stdout).into_owned();

        struct RestoreClipboard(String);
        impl Drop for RestoreClipboard {
            fn drop(&mut self) {
                super::write_to_macos_clipboard(&self.0).ok();
            }
        }
        let _restore = RestoreClipboard(previous);

        let probe = format!("whispa-clipboard-probe-çã-{}", std::process::id());
        super::write_to_macos_clipboard(&probe).expect("escrever e reler clipboard no macOS");
    }

    #[test]
    fn toggles_cannot_start_recording_during_transcription() {
        let state = super::AppState::new_for_test();
        assert!(state.can_start_new_recording(), "deve permitir gravação em estado inicial");
        state.busy.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(!state.can_start_new_recording(), "não pode gravar enquanto ocupado com transcrição");
    }

    #[test]
    fn clipboard_failure_retries_only_text_copying() {
        let state = super::AppState::new_for_test();
        *state.recovery.lock().unwrap() = Some(super::Recovery::Text("transcrição de teste".into()));
        let error = super::Failure::new(
            super::ErrorKind::Clipboard,
            "Não foi possível copiar",
            "Tente novamente para copiar",
            true,
        );
        let hud = state.build_failure_hud(&error, "detalhe");
        assert!(hud.can_retry, "falha de clipboard deve permitir tentar novamente");
        assert!(hud.has_recovery, "deve indicar que há recuperação disponível");

        let (cycle_id, recovery) = state.prepare_retry().expect("retry deve ser aceito");
        assert_eq!(cycle_id, 1);
        match recovery {
            super::Recovery::Text(text) => assert_eq!(text, "transcrição de teste"),
            super::Recovery::Audio(_) => panic!("não deve reenviar áudio quando a falha for de clipboard"),
        }
    }

    #[test]
    fn failed_audio_survives_repeated_attempts_until_dismissed() {
        let state = super::AppState::new_for_test();
        let audio = super::PendingAudio {
            bytes: std::sync::Arc::new(vec![1, 2, 3, 4]),
            duration_seconds: 12.5,
            limit_reached: false,
        };
        *state.recovery.lock().unwrap() = Some(super::Recovery::Audio(audio.clone()));

        // Primeira falha: timeout
        let timeout_err = super::Failure::new(
            super::ErrorKind::Timeout,
            "Tempo esgotado",
            "Demorou demais",
            true,
        );
        let hud = state.build_failure_hud(&timeout_err, "tentativa 1");
        assert!(hud.can_retry);
        assert!(hud.has_recovery);

        // Prepara tentativa 1
        let (cycle1, rec1) = state.prepare_retry().unwrap();
        assert_eq!(cycle1, 1);
        match rec1 {
            super::Recovery::Audio(a) => assert_eq!(a.bytes.len(), 4),
            _ => panic!("deve ser áudio"),
        }

        // Segunda falha: erro de rede na nova tentativa
        let conn_err = super::Failure::new(
            super::ErrorKind::Connection,
            "Falha de conexão",
            "Sem rede",
            true,
        );
        let hud2 = state.build_failure_hud(&conn_err, "tentativa 2");
        assert!(hud2.can_retry);
        assert!(hud2.has_recovery);

        // Prepara tentativa 2 (o áudio original continua intacto na memória)
        let (cycle2, rec2) = state.prepare_retry().unwrap();
        assert_eq!(cycle2, 2);
        match rec2 {
            super::Recovery::Audio(a) => assert_eq!(a.bytes.len(), 4),
            _ => panic!("deve continuar sendo o áudio original"),
        }

        // Usuário descarta
        state.busy.store(false, std::sync::atomic::Ordering::SeqCst);
        state.perform_dismiss().unwrap();
        assert!(state.recovery.lock().unwrap().is_none(), "áudio deve ser descartado da memória");
        assert_eq!(state.hud.lock().unwrap().state, "idle");
        assert!(!state.hud.lock().unwrap().can_retry);
        assert!(state.can_start_new_recording(), "deve poder iniciar novo ciclo após descartar");
    }

    #[test]
    fn non_retryable_failure_blocks_retry() {
        let state = super::AppState::new_for_test();
        let audio = super::PendingAudio {
            bytes: std::sync::Arc::new(vec![1, 2, 3]),
            duration_seconds: 5.0,
            limit_reached: false,
        };
        *state.recovery.lock().unwrap() = Some(super::Recovery::Audio(audio));

        let non_retryable = super::Failure::new(
            super::ErrorKind::NoSpeech,
            "Nenhuma fala reconhecida",
            "Fale mais perto",
            false,
        );
        let hud = state.build_failure_hud(&non_retryable, "");
        assert!(!hud.can_retry, "erro não-retryable não pode permitir retry");
        assert!(state.prepare_retry().is_err(), "prepare_retry deve rejeitar");
    }
}

#[tauri::command]
fn toggle_command_hint() -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "whispa".into());
    format!("{} --toggle", exe.display())
}

#[tauri::command]
fn open_keyboard_settings() {
    let _ = std::process::Command::new("gnome-control-center")
        .arg("keyboard")
        .spawn();
}

#[tauri::command]
fn save_api_key(app: AppHandle, provider: String, key: String) -> Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("chave vazia".into());
    }

    let path = api_key_path(&app, &provider);
    std::fs::write(&path, trimmed).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    }

    let state = app.state::<AppState>();
    maybe_hide_setup_window(&app, &state);
    Ok(())
}

#[tauri::command]
fn api_key_configured(app: AppHandle, provider: String) -> bool {
    has_api_key(&app, &provider)
}

#[tauri::command]
fn hotkey_confirmed(app: AppHandle) -> bool {
    setup_marker_path(&app).exists()
}

#[tauri::command]
fn platform_name() -> &'static str {
    std::env::consts::OS
}

#[tauri::command]
fn hotkey_display_name() -> &'static str {
    HOTKEY_DISPLAY_NAME
}

// Windows e macOS registram o atalho global de verdade via API nativa do SO
// — diferente do Linux/GNOME, que precisa do fluxo guiado (ver setup-guide).
// Combinação escolhida pra evitar conflito com atalhos comuns de navegador
// (Ctrl/Cmd+T, Cmd+Shift+T já são "nova aba"/"reabrir aba"); não verificado
// em hardware Windows/Mac real ainda.
#[cfg(not(target_os = "linux"))]
const NATIVE_HOTKEY: &str = "Alt+Shift+D";

#[cfg(not(target_os = "linux"))]
fn native_hotkey_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::ShortcutState;

    tauri_plugin_global_shortcut::Builder::new()
        .with_shortcut(NATIVE_HOTKEY)
        .expect("atalho global nativo invalido")
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_recording(app);
            }
        })
        .build()
}

#[tauri::command]
fn list_provider_models() -> Vec<ModelInfo> {
    MODELS.to_vec()
}

#[tauri::command]
fn get_provider_settings(app: AppHandle) -> ProviderSettings {
    read_settings(&app)
}

#[tauri::command]
fn save_provider_settings(app: AppHandle, provider: String, model: String) -> Result<(), String> {
    write_settings(&app, &ProviderSettings { provider, model })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if is_toggle_request(&argv) {
                toggle_recording(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    #[cfg(not(target_os = "linux"))]
    let builder = builder.plugin(native_hotkey_plugin());

    builder
        .manage(AppState {
            recording: Mutex::new(None),
            setup_done: AtomicBool::new(false),
            cycle_id: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            recovery: Mutex::new(None),
            hud: Mutex::new(HudState::new("idle", "")),
        })
        .invoke_handler(tauri::generate_handler![
            toggle_command_hint,
            open_keyboard_settings,
            save_api_key,
            api_key_configured,
            hotkey_confirmed,
            list_provider_models,
            get_provider_settings,
            save_provider_settings,
            platform_name,
            hotkey_display_name,
            get_hud_state,
            retry_transcription,
            dismiss_transcription
        ])
        .setup(|app| {
            let handle = app.handle();
            let state = handle.state::<AppState>();

            // No Linux, só confirmamos o atalho quando o GNOME de fato o
            // disparar (primeiro --toggle real). Em Windows/Mac o registro
            // nativo já aconteceu (ou falhou) na hora de montar o `builder`,
            // então não há passo manual do usuário pra aguardar.
            #[cfg(target_os = "linux")]
            let hotkey_already_confirmed = setup_marker_path(handle).exists();
            #[cfg(not(target_os = "linux"))]
            let hotkey_already_confirmed = true;

            state
                .setup_done
                .store(hotkey_already_confirmed, Ordering::SeqCst);
            #[cfg(not(target_os = "linux"))]
            std::fs::write(setup_marker_path(handle), b"1").ok();

            if let Some(setup_window) = app.get_webview_window(SETUP_WINDOW_LABEL) {
                if hotkey_already_confirmed && has_active_api_key(handle) {
                    setup_window.hide()?;
                } else {
                    setup_window.show()?;
                }
            }

            let settings_item =
                MenuItem::with_id(app, "settings", "Configurações", true, None::<&str>)?;
            let about_item = MenuItem::with_id(app, "about", "Sobre", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings_item, &about_item, &quit_item])?;
            TrayIconBuilder::with_id("main")
                .icon(Image::from_bytes(IDLE_ICON)?)
                .tooltip(format!("whispa — {HOTKEY_DISPLAY_NAME} para ditar"))
                .menu(&menu)
                .on_menu_event(|app, event| {
                    let window_label = match event.id().as_ref() {
                        "quit" => {
                            app.exit(0);
                            return;
                        }
                        "settings" => SETUP_WINDOW_LABEL,
                        "about" => ABOUT_WINDOW_LABEL,
                        _ => return,
                    };
                    if let Some(window) = app.get_webview_window(window_label) {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                })
                .build(app)?;

            if is_toggle_request(&std::env::args().collect::<Vec<_>>()) {
                toggle_recording(handle);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == SETUP_WINDOW_LABEL || window.label() == ABOUT_WINDOW_LABEL {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
