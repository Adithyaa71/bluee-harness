"""Local speech worker - STT and TTS, on this machine, no cloud.

Why a separate process
----------------------
The models are Python (faster-whisper / CTranslate2, Piper and Kokoro over ONNX
Runtime) and they hold hundreds of megabytes of weights on a GPU. Loading that
into the harness would mean the Rust binary could never be rebuilt without
dropping the models, and a model that segfaults would take the assistant with
it. So this runs as a child the harness owns and can restart - the same shape
as `src/browser.rs` and the MCP tool servers.

Why plain http.server and not FastAPI
-------------------------------------
Two endpoints and a health check. The venv already carries enough weight for
the tool servers; a web framework here would earn nothing.

Why ONNX Runtime for TTS
------------------------
It is already installed for the embedding model, and it makes the GPU story for
someone else's machine a single line - `pip install onnxruntime-gpu` - rather
than matching a CUDA build of torch. STT uses faster-whisper (CTranslate2)
because that is what actually runs Whisper fast on a consumer GPU, and it ships
prebuilt wheels.

Protocol (all on 127.0.0.1, no auth - it is a child process, not a service):
  GET  /health          -> what is installed, what device, what is loaded
  GET  /voices          -> voices the configured TTS engine can produce
  POST /stt             -> raw audio bytes in, {"text": ...} out
  POST /tts             -> {"text": ...} in, audio/wav bytes out
  POST /reload          -> drop loaded models (frees VRAM)

Config is read from the JSON file whose path is argv[2], re-read whenever its
mtime changes. The harness owns that file; this process never writes it.
"""

import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8767
CONFIG_PATH = sys.argv[2] if len(sys.argv) > 2 else "data/voice.json"

# Model files live beside the harness's other derived data, not in the venv,
# so they survive a reinstall and are obvious to delete.
MODEL_DIR = os.environ.get("BLUEE_VOICE_MODELS") or os.path.join("data", "voice-models")


# --------------------------------------------------------------------------
# config


class Config:
    """Re-read on mtime change, so saving in the UI takes effect without a
    restart. Deliberately tolerant: a half-written file must not kill the
    worker, so a parse failure keeps the previous good config."""

    def __init__(self, path):
        self.path = path
        self.mtime = 0
        self.data = {}
        self.load()

    def load(self):
        try:
            m = os.path.getmtime(self.path)
        except OSError:
            return
        if m == self.mtime:
            return
        try:
            with open(self.path, "r", encoding="utf-8") as f:
                self.data = json.load(f)
            self.mtime = m
        except Exception:
            pass  # keep the last good one

    def stt(self):
        self.load()
        return self.data.get("stt", {}) or {}

    def tts(self):
        self.load()
        return self.data.get("tts", {}) or {}


CFG = Config(CONFIG_PATH)


def device_for(want):
    """`auto` resolves to cuda when a usable GPU is actually present, which is
    checked by asking the runtime rather than by looking for nvidia-smi."""
    if want in ("cpu", "cuda"):
        return want
    try:
        import ctranslate2

        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda"
    except Exception:
        pass
    try:
        import onnxruntime as ort

        if "CUDAExecutionProvider" in ort.get_available_providers():
            return "cuda"
    except Exception:
        pass
    return "cpu"


# --------------------------------------------------------------------------
# speech to text


class Stt:
    def __init__(self):
        self.model = None
        self.key = None
        self.lock = threading.Lock()

    def unload(self):
        with self.lock:
            self.model = None
            self.key = None

    def ensure(self, c):
        want = (c.get("model", "base"), device_for(c.get("device", "auto")),
                c.get("compute_type", "auto"))
        if self.model is not None and self.key == want:
            return self.model
        with self.lock:
            if self.model is not None and self.key == want:
                return self.model
            from faster_whisper import WhisperModel

            name, dev, ct = want
            if ct == "auto":
                # int8 on CPU is several times faster and barely worse; float16
                # is the sensible default on a GPU with limited VRAM.
                ct = "float16" if dev == "cuda" else "int8"
            self.model = WhisperModel(
                name, device=dev, compute_type=ct,
                download_root=os.path.join(MODEL_DIR, "whisper"),
            )
            self.key = want
            return self.model

    def transcribe(self, audio_path, c):
        model = self.ensure(c)
        lang = (c.get("language") or "").strip() or None
        if lang == "auto":
            lang = None
        segments, info = model.transcribe(
            audio_path,
            language=lang,
            task="translate" if c.get("translate") else "transcribe",
            beam_size=int(c.get("beam_size", 5)),
            temperature=float(c.get("temperature", 0.0)),
            initial_prompt=(c.get("prompt") or "").strip() or None,
            vad_filter=bool(c.get("vad", True)),
            vad_parameters={
                "min_silence_duration_ms": int(c.get("vad_silence_ms", 500))
            } if c.get("vad", True) else None,
            condition_on_previous_text=bool(c.get("condition_on_previous", False)),
            no_speech_threshold=float(c.get("no_speech_threshold", 0.6)),
        )
        parts = [s.text for s in segments]
        return {
            "text": "".join(parts).strip(),
            "language": getattr(info, "language", None),
            "language_probability": round(float(getattr(info, "language_probability", 0) or 0), 3),
            "duration": round(float(getattr(info, "duration", 0) or 0), 2),
        }


STT = Stt()


# --------------------------------------------------------------------------
# text to speech


def wav_bytes(samples, rate):
    """Float or int16 samples -> a real WAV container.

    The browser needs a container, not raw PCM, and writing 44 bytes of header
    here is cheaper than adding soundfile to the required set."""
    import numpy as np

    a = np.asarray(samples)
    if a.dtype.kind == "f":
        a = np.clip(a, -1.0, 1.0)
        a = (a * 32767.0).astype("<i2")
    else:
        a = a.astype("<i2")
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(int(rate))
        w.writeframes(a.tobytes())
    return buf.getvalue()


class Tts:
    def __init__(self):
        self.engine = None
        self.key = None
        self.lock = threading.Lock()

    def unload(self):
        with self.lock:
            self.engine = None
            self.key = None

    # ---- piper: tiny ONNX voices, fast on CPU, ~20-60MB each.
    # It takes use_cuda too, so on a GPU box it does not have to sit on the CPU
    # just because it is small enough to.
    def _piper(self, c):
        want = ("piper", c.get("piper_model", ""), bool(c.get("use_cuda")))
        if self.engine is not None and self.key == want:
            return self.engine
        with self.lock:
            from piper import PiperVoice

            path = c.get("piper_model", "")
            if not path:
                raise RuntimeError("no Piper voice file chosen")
            if not os.path.isabs(path):
                path = os.path.join(MODEL_DIR, "piper", path)
            if not os.path.exists(path):
                raise RuntimeError(f"Piper voice not found: {path}")
            cuda = bool(c.get("use_cuda")) and device_for(c.get("device", "auto")) == "cuda"
            self.engine = PiperVoice.load(path, use_cuda=cuda)
            self.key = want
            return self.engine

    # ---- kokoro: 82M params, ~310MB ONNX, markedly better quality than Piper
    def _kokoro(self, c):
        want = ("kokoro", c.get("kokoro_model", ""), c.get("kokoro_voices", ""))
        if self.engine is not None and self.key == want:
            return self.engine
        with self.lock:
            from kokoro_onnx import Kokoro

            m = c.get("kokoro_model") or os.path.join(MODEL_DIR, "kokoro", "kokoro-v1.0.onnx")
            v = c.get("kokoro_voices") or os.path.join(MODEL_DIR, "kokoro", "voices-v1.0.bin")
            for p in (m, v):
                if not os.path.exists(p):
                    raise RuntimeError(f"Kokoro file not found: {p}")
            self.engine = Kokoro(m, v)
            self.key = want
            return self.engine

    def voices(self, c):
        eng = c.get("engine", "piper")
        if eng == "kokoro":
            try:
                k = self._kokoro(c)
                return sorted(k.get_voices())
            except Exception as e:
                return {"error": str(e)}
        if eng == "piper":
            d = os.path.join(MODEL_DIR, "piper")
            try:
                return sorted(f for f in os.listdir(d) if f.endswith(".onnx"))
            except OSError:
                return []
        return []

    def speak(self, text, c):
        eng = c.get("engine", "piper")
        speed = float(c.get("speed", 1.0) or 1.0)

        if eng == "piper":
            v = self._piper(c)
            from piper.config import SynthesisConfig

            # Piper expresses speed as length_scale, where BIGGER is SLOWER -
            # the inverse of how everyone talks about it, so the UI shows speed
            # and the conversion happens here rather than in the reader's head.
            syn = SynthesisConfig(
                length_scale=(1.0 / speed if speed > 0 else 1.0),
                noise_scale=float(c.get("noise_scale", 0.667)),
                noise_w_scale=float(c.get("noise_w", 0.8)),
                volume=float(c.get("volume", 1.0)),
                normalize_audio=bool(c.get("normalize", True)),
                speaker_id=(int(c["speaker_id"]) if str(c.get("speaker_id", "")).strip() else None),
            )
            chunks = [ch.audio_int16_bytes for ch in v.synthesize(text, syn_config=syn)]
            pcm = b"".join(chunks)
            rate = getattr(getattr(v, "config", None), "sample_rate", 22050)
            import numpy as np

            return wav_bytes(np.frombuffer(pcm, dtype="<i2"), rate)

        if eng == "kokoro":
            k = self._kokoro(c)
            samples, rate = k.create(
                text,
                voice=c.get("voice") or "af_heart",
                speed=speed,
                lang=c.get("kokoro_lang") or "en-us",
            )
            return wav_bytes(samples, rate)

        if eng == "command":
            return self._command(text, c)

        raise RuntimeError(f"unknown TTS engine: {eng}")

    # ---- escape hatch: any binary that writes a wav
    def _command(self, text, c):
        cmd = (c.get("command") or "").strip()
        if not cmd:
            raise RuntimeError("no TTS command set")
        out = os.path.join(tempfile.gettempdir(), f"bluee-tts-{os.getpid()}.wav")
        parts = [p.replace("{out}", out) for p in cmd.split()]
        r = subprocess.run(parts, input=text.encode("utf-8"),
                           capture_output=True, timeout=120)
        if not os.path.exists(out):
            raise RuntimeError(
                (r.stderr.decode("utf-8", "replace")[:400] or "command wrote no {out} file"))
        with open(out, "rb") as f:
            data = f.read()
        try:
            os.remove(out)
        except OSError:
            pass
        return data


TTS = Tts()


# --------------------------------------------------------------------------
# http


def installed():
    """What is actually importable. Reported rather than assumed, so the
    settings page can say 'not installed' instead of failing at first use."""
    out = {}
    for name, mod in (("faster_whisper", "faster_whisper"),
                      ("piper", "piper"),
                      ("kokoro_onnx", "kokoro_onnx"),
                      ("onnxruntime", "onnxruntime"),
                      ("numpy", "numpy")):
        try:
            __import__(mod)
            out[name] = True
        except Exception:
            out[name] = False
    return out


def gpu_info():
    info = {"cuda": False, "detail": ""}
    try:
        import ctranslate2

        n = ctranslate2.get_cuda_device_count()
        info["cuda"] = n > 0
        info["detail"] = f"ctranslate2 sees {n} CUDA device(s)"
    except Exception as e:
        info["detail"] = f"ctranslate2 unavailable ({type(e).__name__})"
    try:
        import onnxruntime as ort

        info["onnx_providers"] = ort.get_available_providers()
    except Exception:
        info["onnx_providers"] = []
    return info


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass  # the harness owns the console

    def _send(self, code, body, ctype="application/json"):
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode("utf-8")
        elif isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def do_GET(self):
        try:
            if self.path.startswith("/health"):
                s, t = CFG.stt(), CFG.tts()
                return self._send(200, {
                    "ok": True,
                    "installed": installed(),
                    "gpu": gpu_info(),
                    "stt_device": device_for(s.get("device", "auto")),
                    "stt_loaded": STT.key[0] if STT.key else None,
                    "tts_loaded": TTS.key[0] if TTS.key else None,
                    "model_dir": os.path.abspath(MODEL_DIR),
                    "python": sys.version.split()[0],
                })
            if self.path.startswith("/voices"):
                return self._send(200, {"voices": TTS.voices(CFG.tts())})
            return self._send(404, {"error": "no such endpoint"})
        except Exception as e:
            return self._send(500, {"error": str(e), "trace": traceback.format_exc()[-800:]})

    def do_POST(self):
        try:
            if self.path.startswith("/reload"):
                STT.unload()
                TTS.unload()
                return self._send(200, {"ok": True})

            if self.path.startswith("/stt"):
                raw = self._body()
                if not raw:
                    return self._send(400, {"error": "no audio in the request body"})
                ext = self.headers.get("X-Audio-Ext") or "webm"
                p = os.path.join(tempfile.gettempdir(), f"bluee-stt-{time.time_ns()}.{ext}")
                with open(p, "wb") as f:
                    f.write(raw)
                try:
                    t0 = time.time()
                    out = STT.transcribe(p, CFG.stt())
                    out["ms"] = int((time.time() - t0) * 1000)
                    out["bytes"] = len(raw)
                    return self._send(200, out)
                finally:
                    try:
                        os.remove(p)
                    except OSError:
                        pass

            if self.path.startswith("/tts"):
                req = json.loads(self._body() or b"{}")
                text = (req.get("text") or "").strip()
                if not text:
                    return self._send(400, {"error": "nothing to say"})
                c = dict(CFG.tts())
                for k in ("engine", "voice", "speed"):      # per-call overrides
                    if req.get(k) is not None:
                        c[k] = req[k]
                t0 = time.time()
                wav = TTS.speak(text, c)
                self.send_response(200)
                self.send_header("Content-Type", "audio/wav")
                self.send_header("Content-Length", str(len(wav)))
                self.send_header("X-Synth-Ms", str(int((time.time() - t0) * 1000)))
                self.end_headers()
                self.wfile.write(wav)
                return

            return self._send(404, {"error": "no such endpoint"})
        except Exception as e:
            return self._send(500, {"error": str(e), "trace": traceback.format_exc()[-800:]})


if __name__ == "__main__":
    os.makedirs(MODEL_DIR, exist_ok=True)
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    # Printed so the harness can confirm the child came up, and so running this
    # by hand tells you where it is.
    print(f"bluee voice worker on http://127.0.0.1:{PORT}", flush=True)
    srv.serve_forever()
