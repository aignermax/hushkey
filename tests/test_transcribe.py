"""Batch transcription: file collection, skip-state, note writing (fake model)."""
import sys
import types

import transcribe


def test_collect_files_and_folders(tmp_path):
    (tmp_path / "a.wav").write_bytes(b"x")
    (tmp_path / "b.txt").write_text("no")
    sub = tmp_path / "sub"
    sub.mkdir()
    (sub / "c.mp3").write_bytes(b"x")

    flat = transcribe.collect([str(tmp_path)], recursive=False)
    assert any(p.endswith("a.wav") for p in flat)
    assert not any(p.endswith("c.mp3") for p in flat)

    deep = transcribe.collect([str(tmp_path)], recursive=True)
    assert any(p.endswith("c.mp3") for p in deep)


def test_state_roundtrip(tmp_path):
    path = str(tmp_path / "state.json")
    transcribe.save_state(path, {"a": {"mtime": 1, "size": 2}})
    assert transcribe.load_state(path)["a"]["size"] == 2
    assert transcribe.load_state(str(tmp_path / "missing.json")) == {}


class FakeSegment:
    def __init__(self, text):
        self.text = text


class FakeInfo:
    duration = 1.5
    language = "de"


def install_fake_whisper(monkeypatch, fail=False):
    mod = types.ModuleType("faster_whisper")

    class FakeModel:
        def __init__(self, *args, **kwargs):
            pass

        def transcribe(self, path, language=None, vad_filter=False, beam_size=1):
            if fail:
                raise RuntimeError("boom")
            return [FakeSegment(" Hallo "), FakeSegment(" Welt ")], FakeInfo()

    mod.WhisperModel = FakeModel
    monkeypatch.setitem(sys.modules, "faster_whisper", mod)


def test_main_writes_markdown_and_skips_unchanged(monkeypatch, tmp_path, capsys):
    install_fake_whisper(monkeypatch)
    audio = tmp_path / "memo.wav"
    audio.write_bytes(b"fake audio")
    out = tmp_path / "notes"

    assert transcribe.main([str(audio), "--out", str(out)]) == 0
    note = (out / "memo.md").read_text(encoding="utf-8")
    assert "Hallo Welt" in note
    assert "type: transcript" in note

    assert transcribe.main([str(audio), "--out", str(out)]) == 0
    assert "0 transcribed, 1 unchanged" in capsys.readouterr().out


def test_main_reports_failures(monkeypatch, tmp_path):
    install_fake_whisper(monkeypatch, fail=True)
    audio = tmp_path / "memo.wav"
    audio.write_bytes(b"fake audio")
    assert transcribe.main([str(audio), "--out", str(tmp_path / "notes")]) == 1
