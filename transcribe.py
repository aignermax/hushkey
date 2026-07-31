#!/usr/bin/env python3
"""Batch-transcribe audio files to Markdown notes, fully local (faster-whisper).

Usage:
  transcribe.py AUDIO_DIR [--out DIR]          # all new/changed files in a folder
  transcribe.py voice.m4a [more.wav] [--out DIR]

- Each audio file becomes one Markdown note with frontmatter (source, model,
  language, duration) and the transcript as body.
- Already-transcribed files are skipped (tracked by mtime+size in
  <out>/.transcribe-state.json). Delete that file to force a redo.
- GPU (CUDA) is used when available, otherwise CPU. After the one-time model
  download nothing leaves the machine.

Options:
  --out DIR       output folder (default: <AUDIO_DIR>/transcripts or ./transcripts)
  --language XX   language code (default: de; empty string = auto-detect)
  --model NAME    whisper model (default: medium on GPU, small on CPU)
  --recursive     also scan subfolders
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import sys
from datetime import datetime

SUPPORTED_EXT = {".wav", ".mp3", ".m4a", ".ogg", ".opus", ".flac",
                 ".webm", ".mp4", ".aac", ".wma"}


def preload_cuda_libs():
    try:
        import ctypes
        import nvidia.cublas  # type: ignore
        import nvidia.cudnn  # type: ignore
        libs = []
        for pkg in (nvidia.cublas, nvidia.cudnn):
            pkg_dir = pkg.__path__[0] if pkg.__file__ is None \
                else os.path.dirname(pkg.__file__)
            if sys.platform == "win32":
                libs += sorted(glob.glob(os.path.join(pkg_dir, "bin", "*.dll")))
            else:
                libs += sorted(glob.glob(os.path.join(pkg_dir, "lib", "*.so*")))
        for lib in libs:
            if hasattr(os, "add_dll_directory"):  # Windows Python 3.8+
                os.add_dll_directory(os.path.dirname(lib))
            try:
                ctypes.CDLL(lib, mode=getattr(ctypes, "RTLD_GLOBAL", 0))
            except OSError:
                pass
    except ImportError:
        pass


def pick_device():
    preload_cuda_libs()
    try:
        import ctranslate2  # type: ignore
        if ctranslate2.get_cuda_device_count() > 0:
            return "cuda", "float16", "medium"
    except Exception:
        pass
    return "cpu", "int8", "small"


def load_state(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, json.JSONDecodeError):
        return {}


def save_state(path, state):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, indent=1)
    os.replace(tmp, path)


def collect(inputs, recursive):
    files = []
    for item in inputs:
        if os.path.isdir(item):
            pattern = os.path.join(item, "**" if recursive else "*")
            for path in sorted(glob.glob(pattern, recursive=recursive)):
                if os.path.isfile(path) and \
                        os.path.splitext(path)[1].lower() in SUPPORTED_EXT:
                    files.append(os.path.abspath(path))
        elif os.path.isfile(item):
            files.append(os.path.abspath(item))
        else:
            print(f"warning: not found: {item}", file=sys.stderr)
    return files


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inputs", nargs="+", help="audio files and/or folders")
    ap.add_argument("--out", help="output folder for Markdown notes")
    ap.add_argument("--language", default=os.environ.get("WHISPER_LANG", "de"))
    ap.add_argument("--model", default=os.environ.get("WHISPER_MODEL"))
    ap.add_argument("--recursive", action="store_true")
    args = ap.parse_args(argv)

    files = collect(args.inputs, args.recursive)
    if not files:
        print("no audio files found")
        return 0
    out_dir = args.out or os.path.join(
        args.inputs[0] if os.path.isdir(args.inputs[0]) else os.getcwd(),
        "transcripts")
    os.makedirs(out_dir, exist_ok=True)
    state_path = os.path.join(out_dir, ".transcribe-state.json")
    state = load_state(state_path)

    device, compute, default_model = pick_device()
    model_name = args.model or default_model
    preload_cuda_libs()
    from faster_whisper import WhisperModel
    print(f"loading model '{model_name}' on {device} ({compute}) — first run downloads it",
          file=sys.stderr)
    try:
        model = WhisperModel(model_name, device=device, compute_type=compute)
    except Exception as exc:
        if device == "cuda":
            print(f"CUDA failed ({exc}), falling back to CPU", file=sys.stderr)
            device, compute = "cpu", "int8"
            model = WhisperModel(model_name if args.model else "small",
                                 device=device, compute_type=compute)
        else:
            raise

    failures = skipped = done = 0
    for path in files:
        st = os.stat(path)
        rec = state.get(path)
        if rec and rec.get("mtime") == st.st_mtime and rec.get("size") == st.st_size:
            skipped += 1
            continue
        print(f"transcribing {path} ...", file=sys.stderr)
        try:
            segments, info = model.transcribe(
                path, language=args.language or None, vad_filter=True, beam_size=5)
            text = " ".join(s.text.strip() for s in segments).strip()
        except Exception as exc:
            print(f"  FAILED: {exc}", file=sys.stderr)
            failures += 1
            continue
        stem = os.path.splitext(os.path.basename(path))[0]
        out_path = os.path.join(out_dir, stem + ".md")
        duration = getattr(info, "duration", None)
        note = f"""---
type: transcript
created: {datetime.now():%Y-%m-%d}
source: {path}
model: {model_name}
language: {getattr(info, 'language', args.language)}
duration_seconds: {int(duration) if duration else ""}
---
# Transcript: {stem}

{text or "(no speech recognized)"}
"""
        with open(out_path, "w", encoding="utf-8") as fh:
            fh.write(note)
        state[path] = {"mtime": st.st_mtime, "size": st.st_size, "out": out_path}
        save_state(state_path, state)
        done += 1
        print(f"  -> {out_path}")
    print(f"done: {done} transcribed, {skipped} unchanged, {failures} failed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
