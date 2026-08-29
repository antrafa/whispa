use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{image::Image, AppHandle, Emitter, Manager, PhysicalPosition};
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

fn show_hud(app: &AppHandle, hud_state: &str) {
    let _ = app.emit("hud-state", serde_json::json!({ "state": hud_state }));
    let main_thread_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = main_thread_app.get_webview_window(HUD_WINDOW_LABEL) else {
            return;
        };
        position_hud(&window);
        let _ = window.show();
    });
}

fn hide_hud_after(app: &AppHandle, expected_cycle_id: u64, delay: std::time::Duration) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let state = app.state::<AppState>();
        if state.cycle_id.load(Ordering::SeqCst) != expected_cycle_id {
            return; // um novo ciclo já começou, não esconde o HUD dele
        }
        let main_thread_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(window) = main_thread_app.get_webview_window(HUD_WINDOW_LABEL) {
                let _ = window.hide();
            }
        });
    });
}

fn end_cycle(app: &AppHandle, cycle_id: u64, success: bool, clipboard_message: Option<&str>) {
    if let Some(message) = clipboard_message {
        write_to_clipboard(app, message);
    }
    set_tray_state(app, false);
    show_hud(app, if success { "success" } else { "error" });
    hide_hud_after(app, cycle_id, std::time::Duration::from_millis(1400));
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

    std::thread::spawn(move || {
        record_until_stopped(&thread_samples, &thread_format, &thread_stop_flag);
        // Cobre tanto o corte por MAX_RECORDING quanto qualquer saida
        // antecipada (erro de microfone): sem isso, o proximo toggle
        // interpretaria o app como "ainda gravando" pra sempre.
        thread_app
            .state::<AppState>()
            .recording
            .lock()
            .unwrap()
            .take();
        finish_recording(&thread_app, cycle_id, &thread_samples, &thread_format);
    });

    *state.recording.lock().unwrap() = Some(RecordingHandle { stop_flag });
    set_tray_state(app, true);
    show_hud(app, "recording");
}

fn stop_recording(app: &AppHandle, state: &AppState) {
    if let Some(handle) = state.recording.lock().unwrap().take() {
        handle.stop_flag.store(true, Ordering::SeqCst);
    }
    show_hud(app, "processing");
}

fn toggle_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    let is_recording = state.recording.lock().unwrap().is_some();
    if is_recording {
        stop_recording(app, &state);
    } else {
        start_recording(app, &state);
    }
}

fn record_until_stopped(
    samples: &Arc<Mutex<Vec<f32>>>,
    format_out: &Arc<Mutex<Option<AudioFormat>>>,
    stop_flag: &Arc<AtomicBool>,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        eprintln!("whispa: nenhum microfone encontrado");
        return;
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("whispa: falha ao ler configuracao do microfone: {e}");
            return;
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
            return;
        }
    };

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            eprintln!("whispa: falha ao abrir stream de audio: {e}");
            return;
        }
    };

    if let Err(e) = stream.play() {
        eprintln!("whispa: falha ao iniciar gravacao: {e}");
        return;
    }

    let deadline = std::time::Instant::now() + MAX_RECORDING_DURATION;
    while !stop_flag.load(Ordering::SeqCst) {
        if std::time::Instant::now() >= deadline {
            eprintln!("whispa: gravacao cortada em {MAX_RECORDING_DURATION:?} (limite de seguranca)");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    // stream é dropado aqui, o que encerra a captura
}

fn finish_recording(
    app: &AppHandle,
    cycle_id: u64,
    samples: &Arc<Mutex<Vec<f32>>>,
    format: &Arc<Mutex<Option<AudioFormat>>>,
) {
    let samples = samples.lock().unwrap();
    let format = format.lock().unwrap();

    if samples.is_empty() {
        end_cycle(app, cycle_id, false, Some("[whispa] nenhuma fala capturada"));
        return;
    }
    let Some(format) = format.as_ref() else {
        end_cycle(app, cycle_id, false, Some("[whispa] microfone nao respondeu"));
        return;
    };

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let wav_path = std::env::temp_dir().join(format!("whispa-{timestamp}.wav"));

    let spec = hound::WavSpec {
        channels: format.channels,
        sample_rate: format.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    match hound::WavWriter::create(&wav_path, spec) {
        Ok(mut writer) => {
            for sample in samples.iter() {
                writer.write_sample(*sample).ok();
            }
            writer.finalize().ok();
            transcribe_and_deliver(app, cycle_id, &wav_path);
        }
        Err(e) => {
            eprintln!("whispa: falha ao salvar wav: {e}");
            end_cycle(app, cycle_id, false, Some("[whispa] falha ao salvar audio gravado"));
        }
    }
}

fn transcribe_and_deliver(app: &AppHandle, cycle_id: u64, wav_path: &std::path::Path) {
    let settings = read_settings(app);
    let Some(api_key) = read_api_key(app, &settings.provider) else {
        end_cycle(
            app,
            cycle_id,
            false,
            Some("[whispa] configure a chave de API do provedor na janela de setup"),
        );
        return;
    };

    match transcribe(&settings, &api_key, wav_path) {
        Ok(text) => {
            std::fs::remove_file(wav_path).ok();
            end_cycle(app, cycle_id, true, Some(&text));
        }
        Err(e) => {
            eprintln!("whispa: falha na transcricao: {e}");
            end_cycle(app, cycle_id, false, Some("[whispa] falha ao transcrever, tente novamente"));
        }
    }
}

fn transcribe(
    settings: &ProviderSettings,
    api_key: &str,
    wav_path: &std::path::Path,
) -> Result<String, String> {
    tauri::async_runtime::block_on(transcribe_async(
        provider_base_url(&settings.provider),
        api_key,
        &settings.model,
        wav_path,
    ))
}

async fn transcribe_async(
    base_url: &str,
    api_key: &str,
    model: &str,
    wav_path: &std::path::Path,
) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct TranscriptionResponse {
        text: String,
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;

    let file_bytes = std::fs::read(wav_path).map_err(|e| format!("falha ao ler audio: {e}"))?;
    let file_name = wav_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio.wav".to_string());
    let part = reqwest::multipart::Part::bytes(file_bytes)
        .file_name(file_name)
        .mime_str("audio/wav")
        .map_err(|e| format!("falha ao anexar audio: {e}"))?;

    let form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("language", "pt")
        .part("file", part);

    let response = client
        .post(base_url)
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("falha na chamada ao provedor: {e:?}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("provedor respondeu {status}: {body}"));
    }

    response
        .json::<TranscriptionResponse>()
        .await
        .map(|r| r.text)
        .map_err(|e| format!("resposta invalida do provedor: {e}"))
}

fn write_to_clipboard(app: &AppHandle, text: &str) {
    let _ = app.clipboard().write_text(text.to_string());
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
            hotkey_display_name
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
