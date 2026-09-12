use std::{io::Cursor, time::Duration};

pub const MAX_UPLOAD_BYTES: usize = 24_000_000;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Timeout, Connection, Credentials, Quota, RateLimit, TooLarge,
    Provider, InvalidResponse, NoSpeech, Audio, Clipboard,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Failure {
    pub kind: ErrorKind,
    pub title: String,
    pub message: String,
    pub retryable: bool,
}

impl Failure {
    pub fn new(kind: ErrorKind, title: &str, message: &str, retryable: bool) -> Self {
        Self { kind, title: title.into(), message: message.into(), retryable }
    }
}

pub fn encode_audio(samples: &[f32], sample_rate: u32, channels: u16) -> Result<Vec<u8>, Failure> {
    let invalid = || Failure::new(ErrorKind::Audio, "Áudio não capturado", "Confira o microfone e a permissão de gravação nos ajustes do sistema.", false);
    if samples.is_empty() || sample_rate == 0 || channels == 0 || samples.len() % channels as usize != 0 {
        return Err(invalid());
    }
    let ch = channels as usize;
    let frames = samples.len() / ch;

    // Convert multi-channel samples to mono f32.
    let mono: Vec<f32> = samples.chunks_exact(ch)
        .map(|frame| {
            let sum: f64 = frame.iter().map(|s| if s.is_finite() { *s as f64 } else { 0.0 }).sum();
            (sum / ch as f64).clamp(-1.0, 1.0) as f32
        })
        .collect();

    // Microphones capturing at >48 kHz (like 96 kHz or 192 kHz) produce excessive data
    // (e.g. 180s at 96 kHz is ~34.5 MB mono PCM16) which would exceed provider limits.
    // Resample down to 48 kHz using linear interpolation to preserve the full duration within upload limits.
    let (final_samples, final_rate) = if sample_rate > 48_000 {
        let target_rate = 48_000u32;
        let ratio = sample_rate as f64 / target_rate as f64;
        let target_len = ((frames as f64) / ratio).round() as usize;
        let mut resampled = Vec::with_capacity(target_len);
        for i in 0..target_len {
            let src = i as f64 * ratio;
            let idx0 = src.floor() as usize;
            let idx1 = (idx0 + 1).min(mono.len().saturating_sub(1));
            let frac = (src - idx0 as f64) as f32;
            let val = mono[idx0] * (1.0 - frac) + mono[idx1] * frac;
            resampled.push(val);
        }
        (resampled, target_rate)
    } else {
        (mono, sample_rate)
    };

    if final_samples.len() > (MAX_UPLOAD_BYTES - 44) / 2 {
        return Err(http_failure(413, ""));
    }
    let spec = hound::WavSpec { channels: 1, sample_rate: final_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut cursor = Cursor::new(Vec::with_capacity(44 + final_samples.len() * 2));
    let mut writer = hound::WavWriter::new(&mut cursor, spec).map_err(|_| invalid())?;
    for sample in final_samples {
        let pcm = (sample as f64 * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        writer.write_sample(pcm).map_err(|_| invalid())?;
    }
    writer.finalize().map_err(|_| invalid())?;
    Ok(cursor.into_inner())
}

pub fn http_failure(status: u16, body: &str) -> Failure {
    let code = serde_json::from_str::<serde_json::Value>(body).ok()
        .and_then(|v| v["error"]["code"].as_str().or(v["error"]["type"].as_str()).map(str::to_owned));
    match status {
        401 | 403 => Failure::new(ErrorKind::Credentials, "Confira sua chave de API", "O provedor recusou o acesso. Corrija a chave ou as permissões nas Configurações e tente novamente.", true),
        413 => Failure::new(ErrorKind::TooLarge, "Áudio acima do limite", "O arquivo excede o limite de envio. Grave um trecho menor ou reduza a frequência do microfone nos ajustes de áudio.", false),
        429 if matches!(code.as_deref(), Some("insufficient_quota" | "quota_exceeded" | "billing_hard_limit_reached")) => Failure::new(ErrorKind::Quota, "Saldo ou cota esgotados", "Confira os créditos e os limites da sua conta no provedor. Depois, tente novamente.", true),
        429 => Failure::new(ErrorKind::RateLimit, "Limite de uso atingido", "O provedor limitou as solicitações. Aguarde um pouco antes de tentar novamente.", true),
        408 | 504 => Failure::new(ErrorKind::Timeout, "O provedor demorou demais", "Não recebemos a transcrição a tempo. Confira sua conexão e tente novamente.", true),
        500..=599 => Failure::new(ErrorKind::Provider, "Provedor indisponível", "O serviço de transcrição apresentou uma falha. Aguarde um pouco e tente novamente.", true),
        _ => Failure::new(ErrorKind::Provider, "O provedor recusou o áudio", &format!("Resposta HTTP {status}. Confira o modelo escolhido nas Configurações e tente novamente."), true),
    }
}

fn network_failure(error: reqwest::Error) -> Failure {
    if error.is_timeout() {
        Failure::new(ErrorKind::Timeout, "Tempo de espera esgotado", "A conexão ou a transcrição excedeu o prazo de espera. Confira sua conexão e tente novamente.", true)
    } else {
        Failure::new(ErrorKind::Connection, "Falha de conexão", "Não foi possível concluir a comunicação com o provedor. Confira sua conexão e tente novamente.", true)
    }
}

pub async fn transcribe(base_url: &str, api_key: &str, model: &str, audio: &[u8]) -> Result<String, Failure> {
    if audio.len() > MAX_UPLOAD_BYTES { return Err(http_failure(413, "")); }
    if audio.is_empty() { return Err(Failure::new(ErrorKind::Audio, "Áudio vazio", "Grave novamente e confira seu microfone.", false)); }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .build().map_err(network_failure)?;
    let part = reqwest::multipart::Part::bytes(audio.to_vec())
        .file_name("audio.wav").mime_str("audio/wav").expect("valid WAV MIME type");
    let form = reqwest::multipart::Form::new()
        .text("model", model.to_string()).text("language", "pt").part("file", part);
    let response = client.post(base_url).bearer_auth(api_key).multipart(form)
        .send().await.map_err(network_failure)?;
    let status = response.status();
    let body = response.text().await.map_err(network_failure)?;
    if !status.is_success() { return Err(http_failure(status.as_u16(), &body)); }
    #[derive(serde::Deserialize)]
    struct Response { text: String }
    let response: Response = serde_json::from_str(&body).map_err(|_| Failure::new(
        ErrorKind::InvalidResponse, "Resposta inválida", "O provedor respondeu sem uma transcrição válida. Tente novamente.", true))?;
    let text = response.text.trim().to_string();
    if text.is_empty() { return Err(Failure::new(ErrorKind::NoSpeech, "Nenhuma fala reconhecida", "Confira o microfone e grave novamente, falando mais perto dele.", false)); }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn respond_once(status: u16, body: &str) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" { break; }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            write!(socket, "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        (url, worker)
    }

    #[test]
    fn http_responses_reach_the_correct_user_error() {
        for (status, body, expected) in [
            (401, "not authorized", ErrorKind::Credentials),
            (429, r#"{"error":{"code":"insufficient_quota"}}"#, ErrorKind::Quota),
            (429, "rate limited", ErrorKind::RateLimit),
            (503, "unavailable", ErrorKind::Provider),
            (200, "invalid json", ErrorKind::InvalidResponse),
            (200, r#"{"text":"   "}"#, ErrorKind::NoSpeech),
        ] {
            let (url, server) = respond_once(status, body);
            let result = tauri::async_runtime::block_on(transcribe(&url, "test-key", "test-model", b"audio"));
            server.join().unwrap();
            assert_eq!(result.unwrap_err().kind, expected);
        }
    }

    #[test]
    fn successful_response_preserves_accents_and_trims_whitespace() {
        let (url, server) = respond_once(200, r#"{"text":"  Olá, descrição completa. \n"}"#);
        let result = tauri::async_runtime::block_on(transcribe(&url, "test-key", "test-model", b"audio"));
        server.join().unwrap();
        assert_eq!(result.unwrap(), "Olá, descrição completa.");
    }

    #[test]
    fn connection_failure_and_oversized_audio_are_distinguishable() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let result = tauri::async_runtime::block_on(transcribe(&url, "test-key", "test-model", b"audio"));
        assert_eq!(result.unwrap_err().kind, ErrorKind::Connection);
        let result = tauri::async_runtime::block_on(transcribe(&url, "test-key", "test-model", &vec![0; 24_000_001]));
        assert_eq!(result.unwrap_err().kind, ErrorKind::TooLarge);
    }

    #[test]
    fn three_minutes_of_stereo_fit_the_upload_limit() {
        let samples = vec![0.25; 48_000 * 2 * 180];
        let bytes = encode_audio(&samples, 48_000, 2).unwrap();
        assert_eq!(bytes.len(), 17_280_044);
        assert!(bytes.len() < 25_000_000);
    }

    #[test]
    fn high_sample_rate_is_resampled_to_48k_and_fits_upload_limit() {
        // Three minutes at 96 kHz stereo: 96,000 * 2 * 180 = 34,560,000 samples.
        // Without downsampling, this would be ~34.5 MB and exceed the 24 MB limit.
        // Resampled to 48 kHz mono PCM16, it fits within 17.28 MB.
        let samples = vec![0.25; 96_000 * 2 * 180];
        let bytes = encode_audio(&samples, 96_000, 2).unwrap();
        assert_eq!(bytes.len(), 17_280_044);
        let wav = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(wav.spec().sample_rate, 48_000);
        assert_eq!(wav.spec().channels, 1);
        assert_eq!(wav.duration(), 48_000 * 180);
    }

    #[test]
    fn pcm_conversion_clips_and_sanitizes_non_finite_samples() {
        let bytes = encode_audio(&[-2.0, -1.0, 0.0, 1.0, 2.0, f32::NAN, f32::INFINITY], 48_000, 1).unwrap();
        let mut wav = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        let samples: Vec<i16> = wav.samples().map(Result::unwrap).collect();
        assert_eq!(samples, [-32768, -32768, 0, 32767, 32767, 0, 0]);
    }

    #[test]
    fn stereo_audio_becomes_small_mono_pcm_without_losing_duration() {
        // One second at 48 kHz: 96,000 samples in stereo -> 96,044 bytes of mono PCM.
        let samples = [0.25_f32, 0.75].repeat(48_000);
        let bytes = encode_audio(&samples, 48_000, 2).unwrap();
        assert_eq!(bytes.len(), 96_044);
        let mut wav = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(wav.duration(), 48_000);
        assert_eq!(wav.spec().channels, 1);
        assert_eq!(wav.samples::<i16>().next().unwrap().unwrap(), 16_384);
    }

    #[test]
    fn invalid_audio_is_rejected_without_panicking() {
        for (samples, rate, channels) in [(vec![], 48_000, 1), (vec![0.5], 0, 1), (vec![0.5], 48_000, 0), (vec![0.5], 48_000, 2)] {
            assert!(encode_audio(&samples, rate, channels).is_err());
        }
    }

    #[test]
    fn provider_errors_explain_the_recovery_without_echoing_response_data() {
        for (status, body, kind, retryable) in [
            (401, "secret", ErrorKind::Credentials, true),
            (403, "secret", ErrorKind::Credentials, true),
            (413, "secret", ErrorKind::TooLarge, false),
            (429, r#"{"error":{"code":"insufficient_quota","message":"secret"}}"#, ErrorKind::Quota, true),
            (429, "secret", ErrorKind::RateLimit, true),
            (503, "secret", ErrorKind::Provider, true),
            (400, "secret", ErrorKind::Provider, true),
        ] {
            let error = http_failure(status, body);
            assert_eq!(error.kind, kind);
            assert_eq!(error.retryable, retryable);
            assert!(!error.message.contains("secret"));
        }
    }
}
