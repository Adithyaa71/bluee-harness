# Voice — local speech in and out

Everything here runs on the machine it is installed on. There is no API key and
no endpoint, because nothing is being called. The LLM is separate and unchanged:
it can be a cloud API on the Providers page while all the speech stays local.

- **Listening** — Whisper, via [faster-whisper](https://github.com/SYSTRAN/faster-whisper) (CTranslate2)
- **Speaking** — [Piper](https://github.com/OHF-Voice/piper1-gpl) (tiny, fast) or
  [Kokoro](https://github.com/thewh1teagle/kokoro-onnx) (~310 MB, noticeably better)
- **Escape hatch** — any binary that reads text on stdin and writes a wav

Configure it all at **Settings → Voice**. Nothing here needs editing by hand.

---

## Install

The packages live in the tool-server venv (`.venv`), next to the MCP servers.

```bash
uv pip install --python .venv/Scripts/python.exe faster-whisper piper-tts kokoro-onnx
```

That is enough to run on the CPU. Measured on an i5-11300H with no GPU:

| | |
|---|---|
| Piper `en_US-amy-low`, short sentence | **65–90 ms** |
| Whisper `tiny`, 3.6 s clip, warm | **786 ms** |
| First call (loads, and may download, the model) | 30–40 s, once |

## Using the GPU

Two separate runtimes, so two separate steps. Neither is required — without
them **device** resolves to `cpu`, and the status strip on the settings page
says which one it actually landed on rather than which one you asked for.

**Whisper** needs a CUDA build of CTranslate2, plus cuDNN 9 and cuBLAS on the
path:

```bash
uv pip install --python .venv/Scripts/python.exe nvidia-cublas-cu12 nvidia-cudnn-cu12
```

**Piper and Kokoro** go through ONNX Runtime:

```bash
uv pip uninstall --python .venv/Scripts/python.exe onnxruntime
uv pip install   --python .venv/Scripts/python.exe onnxruntime-gpu
```

Then set **device: cuda** (or leave it on `auto`) and save. The status strip
should change from `CPU` to `CUDA`.

### What fits in 8 GB

| Model | VRAM | Notes |
|---|---|---|
| Whisper `large-v3`, float16 | ~3 GB | The accuracy you actually want |
| Whisper `distil-large-v3`, float16 | ~1.5 GB | Nearly as good, half the size |
| Whisper `small`, float16 | ~0.6 GB | Fine for clean dictation |
| Kokoro | ~0.35 GB | ONNX; good quality for the size |
| Piper | ~0.05 GB | Or leave it on CPU entirely |

`large-v3` + Kokoro is about 3.4 GB, so an 8 GB card runs both comfortably with
room to spare. If VRAM gets tight, **Unload models** on the settings page frees
it without stopping the worker.

## Getting voices

**Piper** — drop any `.onnx` and its matching `.onnx.json` into
`data/voice-models/piper/` and it appears in the dropdown.

```bash
B=https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/amy/low
curl -L -o data/voice-models/piper/en_US-amy-low.onnx      $B/en_US-amy-low.onnx
curl -L -o data/voice-models/piper/en_US-amy-low.onnx.json $B/en_US-amy-low.onnx.json
```

Browse the rest at [rhasspy/piper-voices](https://huggingface.co/rhasspy/piper-voices).
`medium` voices sound better than `low` and are still only ~60 MB.

**Kokoro** — two files, into `data/voice-models/kokoro/`:

```bash
B=https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0
curl -L -o data/voice-models/kokoro/kokoro-v1.0.onnx $B/kokoro-v1.0.onnx
curl -L -o data/voice-models/kokoro/voices-v1.0.bin  $B/voices-v1.0.bin
```

Then press **List voices** on the settings page to see what it offers
(`af_heart`, `am_michael`, and about fifty more).

**Whisper** downloads itself on first use, into `data/voice-models/whisper/`.

## How it fits together

```
browser  ──mic──▶  POST /api/stt   ──▶ ┐
                                        │  voice/worker.py   ──▶ faster-whisper
browser  ◀─audio─  POST /api/tts   ──▶ ┘  (python, one child)  ──▶ piper / kokoro
```

`src/voice.rs` owns the worker the same way `src/browser.rs` owns Chrome: start
it, adopt one a previous run left behind, kill only what we started. The models
are in a child process so that rebuilding the harness does not drop several
hundred megabytes of loaded weights, and so a model that crashes does not take
the assistant down with it.

`data/voice.json` is written by the settings page and read directly by the
worker, which re-reads it whenever its mtime changes. One file, one writer —
most changes take effect without restarting anything.

## Troubleshooting

**Status strip says `CPU` when you have a GPU.** That is the honest answer, not
a display bug — the worker asks the runtime, and `ctranslate2 sees 0 CUDA
device(s)` means CTranslate2 was not built with CUDA. See *Using the GPU*.

**The mic button is disabled.** Voice is off. Settings → Voice → tick *Voice
enabled* → Save.

**First transcription takes 30+ seconds.** It is downloading and loading the
model. Every call after that is warm; the settings page shows what is loaded.

**"Speak it" reports a time but no sound.** Browsers refuse to play audio in a
window nobody has clicked yet. Click the page once. The synthesis itself
succeeded — the timing shown is real.

**Nothing recognisable.** Whisper `tiny` is genuinely weak. Move up to `small`
or `distil-large-v3` before concluding the microphone is at fault.
