//! Voice: speech-to-text and text-to-speech, running locally (§ Phase 5).
//!
//! # Local, not cloud
//!
//! Everything here runs on the machine. There is no API key and no endpoint to
//! configure, because nothing leaves the box - which is the point: a voice
//! assistant that ships your microphone audio to a third party is a different
//! product from the one this is trying to be.
//!
//! # Why a Python child rather than Rust
//!
//! The models are Python: faster-whisper (CTranslate2) for STT, Piper and
//! Kokoro over ONNX Runtime for TTS. Those are the runtimes that actually go
//! fast on a consumer GPU and ship prebuilt wheels, and the venv that runs the
//! MCP tool servers already exists to host them. Keeping them in a child also
//! means a rebuild of the harness does not drop several hundred megabytes of
//! loaded weights, and a model that crashes does not take the assistant down.
//!
//! Same ownership shape as `src/browser.rs`: find it, start it, adopt one a
//! previous run left behind, kill only what we started.
//!
//! # Config
//!
//! `data/voice.json` is written here and read directly by `voice/worker.py`,
//! which re-reads it whenever the mtime changes. One file, one writer, no
//! second copy to drift.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

fn t_true() -> bool {
    true
}
fn d_stt_model() -> String {
    "base".into()
}
fn d_auto() -> String {
    "auto".into()
}
fn d_beam() -> u32 {
    5
}
fn d_silence() -> u32 {
    500
}
fn d_nospeech() -> f32 {
    0.6
}
fn d_one() -> f32 {
    1.0
}
fn d_noise() -> f32 {
    0.667
}
fn d_noise_w() -> f32 {
    0.8
}
fn d_piper() -> String {
    "piper".into()
}
fn d_whisper() -> String {
    "faster-whisper".into()
}
fn d_port() -> u16 {
    8767
}

/// Speech in. Every knob faster-whisper actually honours, so the settings page
/// is not quietly a subset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttConfig {
    #[serde(default = "d_whisper")]
    pub engine: String,
    /// A size name (`tiny` … `large-v3`, `distil-large-v3`) or a local path.
    #[serde(default = "d_stt_model")]
    pub model: String,
    /// `auto` picks cuda when a GPU is genuinely usable, not when one merely exists.
    #[serde(default = "d_auto")]
    pub device: String,
    /// `auto` means int8 on CPU and float16 on GPU - the two that are right
    /// almost always.
    #[serde(default = "d_auto")]
    pub compute_type: String,
    /// `auto` lets Whisper detect it. A fixed code is faster and stops it
    /// guessing wrong on short clips.
    #[serde(default = "d_auto")]
    pub language: String,
    /// Translate to English instead of transcribing in the spoken language.
    #[serde(default)]
    pub translate: bool,
    #[serde(default = "d_beam")]
    pub beam_size: u32,
    #[serde(default)]
    pub temperature: f32,
    /// Seeds the decoder with vocabulary - names, jargon, spellings it would
    /// otherwise mangle.
    #[serde(default)]
    pub prompt: String,
    /// Drop silence before decoding. Usually a large speedup on real recordings.
    #[serde(default = "t_true")]
    pub vad: bool,
    #[serde(default = "d_silence")]
    pub vad_silence_ms: u32,
    #[serde(default = "d_nospeech")]
    pub no_speech_threshold: f32,
    /// Feeding the previous text back in helps continuity and is also how
    /// Whisper gets into repetition loops. Off by default for that reason.
    #[serde(default)]
    pub condition_on_previous: bool,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            engine: d_whisper(),
            model: d_stt_model(),
            device: d_auto(),
            compute_type: d_auto(),
            language: d_auto(),
            translate: false,
            beam_size: d_beam(),
            temperature: 0.0,
            prompt: String::new(),
            vad: true,
            vad_silence_ms: d_silence(),
            no_speech_threshold: d_nospeech(),
            condition_on_previous: false,
        }
    }
}

/// Speech out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// `piper` | `kokoro` | `command`
    #[serde(default = "d_piper")]
    pub engine: String,
    /// Piper: a `.onnx` filename under `data/voice-models/piper`, or a path.
    #[serde(default)]
    pub piper_model: String,
    /// Piper can use CUDA too - being small is not a reason to make it sit on
    /// the CPU when a GPU is there.
    #[serde(default)]
    pub use_cuda: bool,
    #[serde(default = "d_auto")]
    pub device: String,
    #[serde(default)]
    pub kokoro_model: String,
    #[serde(default)]
    pub kokoro_voices: String,
    #[serde(default)]
    pub kokoro_lang: String,
    /// Kokoro voice name, e.g. `af_heart`.
    #[serde(default)]
    pub voice: String,
    #[serde(default = "d_one")]
    pub speed: f32,
    #[serde(default = "d_one")]
    pub volume: f32,
    /// Piper only: expressiveness, and variation in phoneme lengths.
    #[serde(default = "d_noise")]
    pub noise_scale: f32,
    #[serde(default = "d_noise_w")]
    pub noise_w: f32,
    #[serde(default = "t_true")]
    pub normalize: bool,
    /// Multi-speaker Piper voices only; blank means the voice's own default.
    #[serde(default)]
    pub speaker_id: String,
    /// Escape hatch: any binary that writes a wav to `{out}` and takes text on
    /// stdin. There will always be a newer model than the two built in.
    #[serde(default)]
    pub command: String,
    /// Speak replies as they arrive, without being asked each time.
    #[serde(default)]
    pub autoplay: bool,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            engine: d_piper(),
            piper_model: String::new(),
            use_cuda: false,
            device: d_auto(),
            kokoro_model: String::new(),
            kokoro_voices: String::new(),
            kokoro_lang: "en-us".into(),
            voice: String::new(),
            speed: 1.0,
            volume: 1.0,
            noise_scale: d_noise(),
            noise_w: d_noise_w(),
            normalize: true,
            speaker_id: String::new(),
            command: String::new(),
            autoplay: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VoiceConfig {
    /// Master switch. Off means the worker is never started at all, so the
    /// models cost nothing - no VRAM, no process.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_port")]
    pub port: u16,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub tts: TtsConfig,
}

impl VoiceConfig {
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("voice.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| Self {
                port: d_port(),
                ..Default::default()
            })
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).ok();
        let p = dir.join("voice.json");
        std::fs::write(&p, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("could not write {}", p.display()))?;
        Ok(())
    }
}

struct Live {
    /// `None` when we adopted a worker a previous run left behind.
    child: Option<Child>,
    port: u16,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
    }
}

pub struct Voice {
    live: Mutex<Option<Live>>,
    data_dir: PathBuf,
    root: PathBuf,
}

impl Voice {
    pub fn new(data_dir: &Path) -> Self {
        let data_dir = abs(data_dir);
        Self {
            live: Mutex::new(None),
            root: data_dir
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            data_dir,
        }
    }

    pub fn config(&self) -> VoiceConfig {
        VoiceConfig::load(&self.data_dir)
    }

    /// The interpreter that has the speech packages. The tool-server venv, the
    /// same one UACC's sibling servers use - checked by looking, because a venv
    /// that does not exist should say so rather than failing at first use.
    fn python(&self) -> Option<PathBuf> {
        let p = self.root.join(".venv").join("Scripts").join("python.exe");
        if p.exists() {
            return Some(p);
        }
        let p = self.root.join(".venv").join("bin").join("python");
        if p.exists() {
            return Some(p);
        }
        None
    }

    fn worker_py(&self) -> PathBuf {
        self.root.join("voice").join("worker.py")
    }

    pub fn running(&self) -> bool {
        let mut g = self.live.lock().unwrap();
        match g.as_mut() {
            Some(l) => match l.child.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(None)),
                None => true,
            },
            None => false,
        }
    }

    pub fn stop(&self) {
        std::fs::remove_file(self.data_dir.join("voice-worker.port")).ok();
        *self.live.lock().unwrap() = None;
    }

    async fn ensure(&self) -> Result<u16> {
        let cfg = self.config();
        if !cfg.enabled {
            return Err(anyhow!(
                "voice is switched off - turn it on in Settings → Voice"
            ));
        }
        {
            let mut g = self.live.lock().unwrap();
            if let Some(l) = g.as_mut() {
                let alive = match l.child.as_mut() {
                    Some(c) => matches!(c.try_wait(), Ok(None)),
                    None => true,
                };
                if alive {
                    return Ok(l.port);
                }
                *g = None;
            }
        }

        let probe = reqwest::Client::builder()
            .timeout(Duration::from_millis(700))
            .build()?;

        // Adopt a worker a previous run left behind. `taskkill /F` on rebuild
        // (§12a) does not run destructors, and re-loading a large model just to
        // replace an identical one that is already resident is a waste of both
        // time and VRAM.
        let portfile = self.data_dir.join("voice-worker.port");
        if let Ok(txt) = std::fs::read_to_string(&portfile) {
            if let Ok(old) = txt.trim().parse::<u16>() {
                if probe
                    .get(format!("http://127.0.0.1:{old}/health"))
                    .send()
                    .await
                    .is_ok()
                {
                    *self.live.lock().unwrap() = Some(Live {
                        child: None,
                        port: old,
                    });
                    return Ok(old);
                }
            }
        }

        let py = self.python().ok_or_else(|| {
            anyhow!("no .venv found next to the harness - the speech packages live in the tool-server venv")
        })?;
        let script = self.worker_py();
        if !script.exists() {
            return Err(anyhow!("missing {}", script.display()));
        }

        let port = cfg.port;
        let child = Command::new(&py)
            .arg(&script)
            .arg(port.to_string())
            .arg(self.data_dir.join("voice.json"))
            .current_dir(&self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start {}", script.display()))?;

        for _ in 0..40 {
            if probe
                .get(format!("http://127.0.0.1:{port}/health"))
                .send()
                .await
                .is_ok()
            {
                std::fs::write(&portfile, port.to_string()).ok();
                *self.live.lock().unwrap() = Some(Live {
                    child: Some(child),
                    port,
                });
                return Ok(port);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let mut child = child;
        let _ = child.kill();
        Err(anyhow!(
            "the voice worker started but never answered on port {port} - \
             another process may be using it"
        ))
    }

    fn client(secs: u64) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .timeout(Duration::from_secs(secs))
            .build()?)
    }

    /// What is installed and what device it will use. Answered by the worker,
    /// because only the worker can actually import the packages.
    pub async fn health(&self) -> Value {
        let cfg = self.config();
        let base = json!({
            "enabled": cfg.enabled,
            "running": self.running(),
            "venv": self.python().map(|p| p.display().to_string()),
            "worker": self.worker_py().exists(),
        });
        if !cfg.enabled {
            return base;
        }
        match self.ensure().await {
            Err(e) => {
                let mut b = base;
                b["error"] = json!(e.to_string());
                b
            }
            Ok(port) => {
                let got = Self::client(10)
                    .ok()
                    .and_then(|c| Some(c.get(format!("http://127.0.0.1:{port}/health"))))
                    .unwrap();
                match got.send().await {
                    Ok(r) => match r.json::<Value>().await {
                        Ok(mut v) => {
                            v["enabled"] = json!(cfg.enabled);
                            v["running"] = json!(true);
                            v["venv"] = base["venv"].clone();
                            v
                        }
                        Err(e) => json!({ "error": e.to_string() }),
                    },
                    Err(e) => json!({ "error": e.to_string() }),
                }
            }
        }
    }

    pub async fn voices(&self) -> Result<Value> {
        let port = self.ensure().await?;
        Ok(Self::client(30)?
            .get(format!("http://127.0.0.1:{port}/voices"))
            .send()
            .await?
            .json()
            .await?)
    }

    /// Audio in, text out. `ext` tells the worker what container it is so it
    /// can hand ffmpeg/av a filename it understands.
    pub async fn transcribe(&self, audio: Vec<u8>, ext: &str) -> Result<Value> {
        if audio.is_empty() {
            return Err(anyhow!("no audio was sent"));
        }
        let port = self.ensure().await?;
        // Generous: a first call loads (and may download) the model.
        let r = Self::client(600)?
            .post(format!("http://127.0.0.1:{port}/stt"))
            .header("X-Audio-Ext", ext)
            .body(audio)
            .send()
            .await?;
        let v: Value = r.json().await?;
        if let Some(e) = v.get("error") {
            return Err(anyhow!(e.as_str().unwrap_or("transcription failed").to_string()));
        }
        Ok(v)
    }

    /// Text in, WAV bytes out.
    pub async fn speak(&self, text: &str, over: Value) -> Result<Vec<u8>> {
        let text = text.trim();
        if text.is_empty() {
            return Err(anyhow!("nothing to say"));
        }
        let port = self.ensure().await?;
        let mut body = json!({ "text": text });
        if let Some(o) = over.as_object() {
            for (k, v) in o {
                body[k] = v.clone();
            }
        }
        let r = Self::client(600)?
            .post(format!("http://127.0.0.1:{port}/tts"))
            .json(&body)
            .send()
            .await?;
        if !r.status().is_success() {
            let v: Value = r.json().await.unwrap_or_else(|_| json!({}));
            return Err(anyhow!(v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("speech synthesis failed")
                .to_string()));
        }
        Ok(r.bytes().await?.to_vec())
    }

    /// Drop loaded models without stopping the worker - the way to get VRAM
    /// back without losing the process.
    pub async fn unload(&self) -> Result<()> {
        let port = self.ensure().await?;
        Self::client(30)?
            .post(format!("http://127.0.0.1:{port}/reload"))
            .send()
            .await?;
        Ok(())
    }

    /// Piper voices already on disk. Listed from the filesystem rather than the
    /// worker so it answers even when voice is switched off.
    pub fn local_piper_voices(&self) -> Vec<String> {
        let dir = self.data_dir.join("voice-models").join("piper");
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| n.ends_with(".onnx"))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}

/// The worker is started with `current_dir` at the repo root, but the config
/// path is handed over absolute - `cfg.data_dir` defaults to a relative "data",
/// and a relative path that resolves differently in the child is the same class
/// of bug that made Chrome silently refuse its profile directory (§28).
fn abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|c| c.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_safe_ones() {
        let c = VoiceConfig::default();
        // Off, so no model is ever loaded by merely having the feature.
        assert!(!c.enabled);
        assert!(!c.tts.autoplay);
        // VAD on and no conditioning: the pair that avoids Whisper's
        // repetition loops on real recordings.
        assert!(c.stt.vad);
        assert!(!c.stt.condition_on_previous);
        assert_eq!(c.stt.device, "auto");
        assert_eq!(c.stt.compute_type, "auto");
    }

    #[test]
    fn round_trips_through_the_file_the_worker_reads() {
        let dir = std::env::temp_dir().join(format!("voice-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = VoiceConfig::default();
        c.enabled = true;
        c.stt.model = "large-v3".into();
        c.stt.language = "en".into();
        c.tts.engine = "kokoro".into();
        c.tts.voice = "af_heart".into();
        c.tts.speed = 1.25;
        c.save(&dir).unwrap();

        let back = VoiceConfig::load(&dir);
        assert!(back.enabled);
        assert_eq!(back.stt.model, "large-v3");
        assert_eq!(back.tts.voice, "af_heart");
        assert!((back.tts.speed - 1.25).abs() < 1e-6);

        // The worker reads this same file, so the shape it expects must survive.
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("voice.json")).unwrap()).unwrap();
        assert_eq!(raw["stt"]["model"], "large-v3");
        assert_eq!(raw["tts"]["engine"], "kokoro");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_config_yields_usable_defaults_not_an_error() {
        let dir = std::env::temp_dir().join(format!("voice-none-{}", uuid::Uuid::new_v4()));
        let c = VoiceConfig::load(&dir);
        assert!(!c.enabled);
        assert_eq!(c.port, 8767);
    }

    #[test]
    fn a_relative_data_dir_is_made_absolute() {
        let v = Voice::new(Path::new("data"));
        assert!(v.data_dir.is_absolute());
    }
}
