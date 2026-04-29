"""Tests for build-demo.py utility functions."""
import subprocess
from pathlib import Path

import pytest

# Add scripts/ to path so we can import build_demo
import importlib.util
import sys

_spec = importlib.util.spec_from_file_location(
    "build_demo", Path(__file__).parent / "build-demo.py"
)
assert _spec is not None, "Could not locate build-demo.py next to test file"
_mod = importlib.util.module_from_spec(_spec)  # type: ignore[arg-type]
_spec.loader.exec_module(_mod)  # type: ignore[union-attr]
concatenate_audio = _mod.concatenate_audio
validate_demo_name = _mod.validate_demo_name
parse_segments = _mod.parse_segments
lint_segments = _mod.lint_segments
generate_playwright_skeleton = _mod.generate_playwright_skeleton


# ---------------------------------------------------------------------------
# validate_demo_name
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("name", ["redirect-epic", "demo_v2", "MyDemo123"])
def test_validate_demo_name_accepts_safe_names(name: str) -> None:
    assert validate_demo_name(name) == name


@pytest.mark.parametrize(
    "name",
    ["has space", "with'quote", "semi;colon", "back`tick", ""],
)
def test_validate_demo_name_rejects_unsafe_names(name: str) -> None:
    import argparse
    with pytest.raises(argparse.ArgumentTypeError):
        validate_demo_name(name)


# ---------------------------------------------------------------------------
# parse_segments
# ---------------------------------------------------------------------------

def test_parse_segments_returns_empty_list_when_no_segment_markers(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    script.write_text("# Introduction\n\nSome text with no segment markers.\n")

    result = parse_segments(script)

    assert result == []


def test_parse_segments_extracts_single_segment(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "Welcome to the demo.\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert result[0]["name"] == "intro"
    assert result[0]["text"] == "Welcome to the demo."


def test_parse_segments_extracts_multiple_segments(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "First segment text.\n"
        "<!-- SEGMENT: closing -->\n"
        "Second segment text.\n"
    )

    result = parse_segments(script)

    assert len(result) == 2
    assert result[0]["name"] == "intro"
    assert result[0]["text"] == "First segment text."
    assert result[1]["name"] == "closing"
    assert result[1]["text"] == "Second segment text."


def test_parse_segments_stops_body_at_markdown_heading(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "Narration text.\n"
        "# This heading is a boundary\n"
        "Content after heading is not part of segment.\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert "heading" not in result[0]["text"]
    assert result[0]["text"] == "Narration text."


def test_parse_segments_stops_body_at_horizontal_rule(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "Narration text.\n"
        "---\n"
        "Content after rule is not part of segment.\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert result[0]["text"] == "Narration text."


def test_parse_segments_strips_inline_html_comment_from_body(tmp_path: Path) -> None:
    # A plain HTML comment that does NOT match a boundary pattern must be
    # stripped so TTS never reads it aloud.
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "Before comment. <!-- author note: cut this later --> After comment.\n"
        "---\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert "author note" not in result[0]["text"], (
        "HTML comment text must be stripped from segment body"
    )
    assert "Before comment." in result[0]["text"]
    assert "After comment." in result[0]["text"]


def test_parse_segments_section_marker_terminates_body(tmp_path: Path) -> None:
    # A <!-- SECTION: ... --> marker is a boundary — it ends the current
    # segment.  Text that follows it belongs to a new structural block, not
    # to the preceding segment.
    script = tmp_path / "narration.md"
    script.write_text(
        "<!-- SEGMENT: intro -->\n"
        "Narration text.\n"
        "<!-- SECTION: metadata -->\n"
        "This line is after the boundary and must not be in intro.\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert result[0]["name"] == "intro"
    assert result[0]["text"] == "Narration text."
    assert "after the boundary" not in result[0]["text"]


def test_parse_segments_excludes_whitespace_only_bodies(tmp_path: Path) -> None:
    script = tmp_path / "narration.md"
    # Segment whose entire body is whitespace after comment stripping
    script.write_text(
        "<!-- SEGMENT: empty -->\n"
        "   \n"
        "<!-- SEGMENT: real -->\n"
        "Actual narration.\n"
    )

    result = parse_segments(script)

    assert len(result) == 1
    assert result[0]["name"] == "real"


# ---------------------------------------------------------------------------
# lint_segments
# ---------------------------------------------------------------------------

def test_lint_segments_returns_zero_for_clean_text(capsys: pytest.CaptureFixture) -> None:
    segments = [{"name": "intro", "text": "Welcome to the demo. This is clean text."}]

    warning_count = lint_segments(segments)

    assert warning_count == 0


def test_lint_segments_warns_on_residual_html_comment(capsys: pytest.CaptureFixture) -> None:
    segments = [{"name": "broken", "text": "Hello <!-- stray comment --> world."}]

    warning_count = lint_segments(segments)
    stderr = capsys.readouterr().err

    assert warning_count == 1
    assert "broken" in stderr
    assert "HTML comment" in stderr


def test_lint_segments_warns_on_markdown_heading_in_body(capsys: pytest.CaptureFixture) -> None:
    segments = [{"name": "bad", "text": "# This heading will be read aloud as 'hash'"}]

    warning_count = lint_segments(segments)
    stderr = capsys.readouterr().err

    assert warning_count == 1
    assert "bad" in stderr
    assert "heading" in stderr


def test_lint_segments_info_on_markdown_inline_syntax_does_not_increment_warning_count(
    capsys: pytest.CaptureFixture,
) -> None:
    # Inline syntax like backticks triggers an INFO message, not a WARN.
    # The return value counts only WARNs, so it must remain 0.
    segments = [{"name": "info_only", "text": "Use `git commit` to save your work."}]

    warning_count = lint_segments(segments)
    stderr = capsys.readouterr().err

    assert warning_count == 0
    assert "INFO" in stderr


def test_lint_segments_counts_multiple_warnings_across_segments(
    capsys: pytest.CaptureFixture,
) -> None:
    segments = [
        {"name": "first", "text": "<!-- stray comment -->"},
        {"name": "second", "text": "# Heading in body"},
    ]

    warning_count = lint_segments(segments)

    assert warning_count == 2


def test_lint_segments_counts_multiple_warnings_within_one_segment(
    capsys: pytest.CaptureFixture,
) -> None:
    # Both an HTML comment and a heading in the same segment = 2 warnings.
    segments = [
        {
            "name": "double",
            "text": "<!-- comment -->\n# Also a heading",
        }
    ]

    warning_count = lint_segments(segments)

    assert warning_count == 2


# ---------------------------------------------------------------------------
# concatenate_audio
# ---------------------------------------------------------------------------

def test_concatenate_audio_cleans_up_concat_list_on_ffmpeg_failure(tmp_path: Path) -> None:
    """concat_list.txt must be removed even when ffmpeg fails.

    The production code catches CalledProcessError, cleans up, then calls
    sys.exit(1) — so the observable exception at the test boundary is
    SystemExit, not CalledProcessError.
    """
    output = tmp_path / "out.mp3"
    bad_paths = [tmp_path / "nonexistent.mp3"]
    bad_paths[0].write_bytes(b"not an mp3")

    with pytest.raises(SystemExit) as exc_info:
        concatenate_audio(bad_paths, output)

    assert exc_info.value.code == 1, (
        "ffmpeg failure must exit with code 1, not a different exit code"
    )
    assert not (tmp_path / "concat_list.txt").exists(), (
        "concat_list.txt must be removed even when ffmpeg fails"
    )
    assert not output.exists(), (
        "output file must not exist when ffmpeg fails"
    )


# ---------------------------------------------------------------------------
# generate_playwright_skeleton
# ---------------------------------------------------------------------------

def _make_timing(names_and_durations: list[tuple[str, float]]) -> list[dict]:
    return [
        {"name": name, "text": f"Text for {name}", "duration": duration}
        for name, duration in names_and_durations
    ]


def test_generate_playwright_skeleton_creates_output_file(tmp_path: Path) -> None:
    timing = _make_timing([("intro", 5.0), ("closing", 3.0)])
    output = tmp_path / "my-demo-demo.spec.ts"

    generate_playwright_skeleton(timing, output, "my-demo", "http://localhost:3000")

    assert output.exists(), "Skeleton file must be created"
    assert output.stat().st_size > 0


def test_generate_playwright_skeleton_adds_buffer_only_to_last_segment(tmp_path: Path) -> None:
    timing = _make_timing([("intro", 5.0), ("middle", 4.0), ("closing", 3.0)])
    output = tmp_path / "demo.spec.ts"

    generate_playwright_skeleton(timing, output, "demo", "http://localhost:3000")

    content = output.read_text()
    # Last segment: 3.0s + 4s buffer = 7000ms
    assert "waitForTimeout(7000)" in content, (
        "Last segment must include 4-second buffer"
    )
    # Non-last segments must use exact duration with no buffer
    assert "waitForTimeout(5000)" in content, (
        "First segment must NOT include buffer"
    )
    assert "waitForTimeout(4000)" in content, (
        "Middle segment must NOT include buffer"
    )


def test_generate_playwright_skeleton_sets_timeout_to_total_plus_30s_margin(
    tmp_path: Path,
) -> None:
    timing = _make_timing([("intro", 10.0), ("closing", 5.0)])
    output = tmp_path / "demo.spec.ts"

    generate_playwright_skeleton(timing, output, "demo", "http://localhost:3000")

    content = output.read_text()
    # total = 15s, margin = 30s → setTimeout(45000)
    assert "setTimeout(45000)" in content, (
        "Test timeout must be (total_narration + 30) * 1000 ms"
    )


def test_generate_playwright_skeleton_embeds_demo_name(tmp_path: Path) -> None:
    timing = _make_timing([("intro", 2.0)])
    output = tmp_path / "my-special-demo.spec.ts"

    generate_playwright_skeleton(timing, output, "my-special-demo", "http://localhost:3000")

    content = output.read_text()
    assert "my-special-demo" in content, (
        "Demo name must appear in the generated skeleton"
    )


def test_generate_playwright_skeleton_embeds_base_url(tmp_path: Path) -> None:
    timing = _make_timing([("intro", 2.0)])
    output = tmp_path / "demo.spec.ts"

    generate_playwright_skeleton(timing, output, "demo", "https://staging.example.com")

    content = output.read_text()
    assert "https://staging.example.com" in content, (
        "Base URL must appear in the generated skeleton"
    )
