#!/usr/bin/env python3
"""Build a narration-synced demo video from a segmented narration script.

Narration-driven recording: generates audio first, measures durations,
then outputs timing data for writing a demo script with matching sleep values.

Usage:
    ELEVENLABS_API_KEY=<key> ELEVENLABS_VOICE_ID=<id> \
      python build-demo.py <narration-script.md> <demo-name> [--output-dir <dir>]

    # Generate a Playwright demo skeleton alongside the timing data:
    ELEVENLABS_API_KEY=<key> ELEVENLABS_VOICE_ID=<id> \
      python build-demo.py <narration-script.md> <demo-name> --playwright

Flow:
    1. Parse narration script into labeled segments
    2. Generate audio for each segment via ElevenLabs
    3. Measure each clip's duration with ffprobe
    4. Write timing JSON file
    5. Concatenate clips into one narration track
    6. (Optional) Generate Playwright demo script skeleton
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen


SEGMENT_BOUNDARY = re.compile(
    r"<!--\s*SEGMENT:\s*(\w+)\s*-->\s*\n"   # opens the segment
    r"(.*?)"                                 # body — non-greedy
    r"(?="                                   # until (lookahead — don't consume)
    r"<!--\s*SEGMENT:"                     #   next SEGMENT marker
    r"|<!--\s*SECTION:"                    #   SECTION metadata (not narration)
    r"|^#{1,6}[ \t]"                       #   markdown heading (# .. ######)
    r"|^---\s*$"                           #   horizontal rule
    r"|\Z"                                 #   end of file
    r")",
    re.DOTALL | re.MULTILINE,
)

HTML_COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)
MARKDOWN_HEADING_IN_BODY = re.compile(r"^#{1,6}[ \t]", re.MULTILINE)
MARKDOWN_INLINE_SYNTAX = re.compile(r"[*`\[\]_]")


def validate_demo_name(value: str) -> str:
    """Argparse type validator: demo name must be alphanumeric, hyphens, underscores."""
    if not re.fullmatch(r"[A-Za-z0-9_-]+", value):
        raise argparse.ArgumentTypeError(
            f"Demo name {value!r} must contain only letters, digits, hyphens, and underscores"
        )
    return value


def parse_segments(script_path: Path) -> list[dict[str, str]]:
    """Parse a narration script into labeled TTS segments.

    A segment's text runs from its `<!-- SEGMENT: name -->` marker to the
    next structural boundary. Boundaries are any of:
      - the next SEGMENT marker
      - a SECTION metadata marker (`<!-- SECTION: ... -->`)
      - a markdown heading (line starting with `#`)
      - a markdown horizontal rule (a line that is only `---`)
      - end of file

    After extraction, HTML comments are stripped so stray `<!-- ... -->`
    blocks inside a segment body don't get read aloud. Remaining markdown
    syntax (asterisks, brackets, backticks) is NOT stripped — TTS will
    pronounce them literally, which is the signal the author needs to
    fix the narration. `--dry-run` surfaces these before spending credits.
    """
    content = script_path.read_text()
    segments: list[dict[str, str]] = []

    for match in SEGMENT_BOUNDARY.finditer(content):
        name = match.group(1)
        raw_body = match.group(2)
        # Strip HTML comments that fell inside the body (e.g. a nested
        # SECTION marker or author-note) so TTS doesn't see them.
        stripped = HTML_COMMENT.sub("", raw_body)
        # Normalize trailing whitespace on each line, collapse run-on blanks.
        lines = [line.rstrip() for line in stripped.splitlines()]
        text = "\n".join(lines).strip()
        if text:
            segments.append({"name": name, "text": text})

    return segments


def lint_segments(segments: list[dict[str, str]]) -> int:
    """Warn about TTS-unsafe content in parsed segments.

    Returns the number of contamination warnings. Caller decides whether
    to block (e.g. in --strict mode) or just surface them.
    """
    warnings = 0
    for seg in segments:
        name, text = seg["name"], seg["text"]
        if HTML_COMMENT.search(text):
            print(
                f"  WARN  segment {name!r}: contains HTML comment — extractor "
                "missed a boundary. TTS will read the comment text.",
                file=sys.stderr,
            )
            warnings += 1
        if MARKDOWN_HEADING_IN_BODY.search(text):
            print(
                f"  WARN  segment {name!r}: contains a markdown heading "
                "(# ...) — TTS will say 'hash'. Move the heading outside "
                "the segment or use a <!-- SECTION: ... --> comment instead.",
                file=sys.stderr,
            )
            warnings += 1
        if MARKDOWN_INLINE_SYNTAX.search(text):
            print(
                f"  INFO  segment {name!r}: contains markdown inline syntax "
                "(`*`, `` ` ``, `[`, `]`, `_`) — TTS reads these literally.",
                file=sys.stderr,
            )
    return warnings


def generate_segment_audio(
    text: str, output_path: Path, api_key: str, voice_id: str
) -> None:
    """Generate audio for a single narration segment via ElevenLabs."""
    url = f"https://api.elevenlabs.io/v1/text-to-speech/{voice_id}"

    payload = json.dumps({
        "text": text,
        "model_id": "eleven_monolingual_v1",
        "voice_settings": {"stability": 0.5, "similarity_boost": 0.75},
    })

    request = Request(
        url,
        data=payload.encode("utf-8"),
        headers={
            "Accept": "audio/mpeg",
            "Content-Type": "application/json",
            "xi-api-key": api_key,
        },
        method="POST",
    )

    try:
        with urlopen(request, timeout=30) as response:
            output_path.parent.mkdir(parents=True, exist_ok=True)
            output_path.write_bytes(response.read())
    except HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        print(f"  ElevenLabs API error (HTTP {exc.code}): {body}", file=sys.stderr)
        sys.exit(1)
    except OSError as exc:
        print(
            f"ERROR: failed to write audio clip {output_path}: {exc}",
            file=sys.stderr,
        )
        sys.exit(1)


def get_audio_duration(path: Path) -> float:
    """Get duration of an audio file in seconds using ffprobe."""
    try:
        result = subprocess.run(
            [
                "ffprobe", "-v", "quiet",
                "-show_entries", "format=duration",
                "-of", "csv=p=0", str(path),
            ],
            capture_output=True, text=True, check=True,
        )
    except FileNotFoundError:
        print(
            "ERROR: ffprobe not found. Install ffmpeg (e.g. brew install ffmpeg).",
            file=sys.stderr,
        )
        sys.exit(1)
    except subprocess.CalledProcessError as exc:
        print(
            f"ERROR: ffprobe failed on {path} (exit {exc.returncode}):\n{exc.stderr}",
            file=sys.stderr,
        )
        sys.exit(1)

    raw = result.stdout.strip()
    try:
        return float(raw)
    except ValueError:
        print(
            f"ERROR: ffprobe returned unexpected output for {path!r}: {raw!r}",
            file=sys.stderr,
        )
        sys.exit(1)


def concatenate_audio(clip_paths: list[Path], output_path: Path) -> None:
    """Concatenate audio clips into a single file using ffmpeg."""
    list_file = output_path.parent / "concat_list.txt"
    list_file.write_text(
        "\n".join(f"file '{p.resolve()}'" for p in clip_paths) + "\n"
    )
    try:
        subprocess.run(
            [
                "ffmpeg", "-y", "-f", "concat", "-safe", "0",
                "-i", str(list_file), "-c", "copy", str(output_path),
            ],
            capture_output=True, check=True,
        )
    except FileNotFoundError:
        list_file.unlink(missing_ok=True)
        print(
            "ERROR: ffmpeg not found. Install ffmpeg (e.g. brew install ffmpeg).",
            file=sys.stderr,
        )
        sys.exit(1)
    except subprocess.CalledProcessError as exc:
        list_file.unlink(missing_ok=True)
        print(
            f"ERROR: ffmpeg failed concatenating audio (exit {exc.returncode}):\n"
            f"{exc.stderr.decode('utf-8', errors='replace') if isinstance(exc.stderr, bytes) else exc.stderr}",
            file=sys.stderr,
        )
        sys.exit(1)
    else:
        list_file.unlink(missing_ok=True)


def _playwright_segment_blocks(timing: list[dict[str, float | str]]) -> str:
    """Build the per-segment waitForTimeout blocks for the Playwright template."""
    segments_code = []
    for i, t in enumerate(timing):
        name = t["name"]
        duration = t["duration"]
        buffer_comment = ""
        buffer_value = 0
        if i == len(timing) - 1:
            buffer_comment = " + 4s buffer"
            buffer_value = 4
        total_ms = round((float(duration) + buffer_value) * 1000)
        segments_code.append(
            f'    // -- {name} ({duration}s{buffer_comment}) --\n'
            f'    // TODO: Add actions for "{name}" segment\n'
            f'    await page.waitForTimeout({total_ms});'
        )
    return "\n\n".join(segments_code)


def _playwright_template(
    demo_name: str,
    base_url: str,
    total: float,
    timeout_ms: int,
    segments_block: str,
) -> str:
    """Render the full Playwright spec file content."""
    return f'''\
/**
 * {demo_name} - Playwright demo recording, timing synced to narration segments
 *
 * Generated by build-demo.py from narration timing data.
 * Total narration: {total:.1f}s
 *
 * Usage:
 *   npx playwright test {demo_name}-demo.spec.ts --workers=1
 */

import {{ test }} from "@playwright/test";
import {{ readFileSync }} from "fs";
import {{ join }} from "path";

interface SegmentTiming {{
  name: string;
  text: string;
  duration: number;
  clip: string;
}}

const TIMING_PATH = join(__dirname, "recordings", "{demo_name}_timing.json");
const timings: SegmentTiming[] = JSON.parse(
  readFileSync(TIMING_PATH, "utf-8"),
);

const BASE_URL = "{base_url}";

test.describe("{demo_name} Demo", () => {{
  test.setTimeout({timeout_ms});

  test.use({{
    video: {{ mode: "on", size: {{ width: 1920, height: 1080 }} }},
    viewport: {{ width: 1920, height: 1080 }},
    launchOptions: {{
      args: [
        "--disable-infobars",
        "--hide-scrollbars",
        "--disable-extensions",
      ],
    }},
  }});

  test("narrated demo", async ({{ page }}) => {{
    await page.goto(BASE_URL);
    await page.waitForLoadState("networkidle");

{segments_block}
  }});
}});
'''


def generate_playwright_skeleton(
    timing: list[dict[str, float | str]],
    output_path: Path,
    demo_name: str,
    base_url: str,
) -> None:
    """Generate a Playwright test skeleton with timing values from narration."""
    segments_block = _playwright_segment_blocks(timing)
    total = sum(float(t["duration"]) for t in timing)
    timeout_ms = round((total + 30) * 1000)  # 30s margin for test timeout
    output_path.write_text(
        _playwright_template(demo_name, base_url, total, timeout_ms, segments_block)
    )


def build_parser() -> argparse.ArgumentParser:
    """Build and return the CLI argument parser."""
    parser = argparse.ArgumentParser(
        description="Build narration-synced demo audio from a segmented script."
    )
    parser.add_argument("script", type=Path, help="Path to narration script (.md)")
    parser.add_argument(
        "name",
        type=validate_demo_name,
        help="Demo name (used for output file naming; alphanumeric, hyphens, underscores only)",
    )
    parser.add_argument(
        "--output-dir", type=Path, default=Path("recordings"),
        help="Directory for output files (default: ./recordings)",
    )
    parser.add_argument(
        "--playwright", action="store_true",
        help="Generate a Playwright demo script skeleton from timing data",
    )
    parser.add_argument(
        "--base-url", default="http://localhost:3000",
        help="Base URL for the Playwright demo (default: http://localhost:3000)",
    )
    parser.add_argument(
        "--dry-run", action="store_true",
        help=(
            "Parse and lint segments, print what would be sent to TTS, then "
            "exit. No ElevenLabs calls, no files written. Use this before "
            "every real run — it's the last line of defense against spending "
            "credits on narration that will say 'hash hash section six'."
        ),
    )
    parser.add_argument(
        "--strict", action="store_true",
        help="Exit non-zero if the linter emits WARN findings.",
    )
    return parser


def run_dry_run(segments: list[dict[str, str]], strict: bool, warnings: int) -> None:
    """Print segment texts that would be sent to TTS, then exit."""
    print("\n--- Dry run: the following text would be sent to TTS ---")
    for seg in segments:
        print(f"\n=== SEGMENT: {seg['name']} ({len(seg['text'])} chars) ===")
        print(seg["text"])
    print("\n--- End of dry run. No audio generated. ---")
    if strict and warnings:
        sys.exit(2)


def validate_env() -> tuple[str, str]:
    """Read and validate ElevenLabs credentials from environment. Exits on failure."""
    api_key = os.environ.get("ELEVENLABS_API_KEY")
    voice_id = os.environ.get("ELEVENLABS_VOICE_ID")
    if not api_key or not voice_id:
        print("ELEVENLABS_API_KEY and ELEVENLABS_VOICE_ID required", file=sys.stderr)
        sys.exit(1)
    if not re.fullmatch(r"[A-Za-z0-9]+", voice_id):
        print("ELEVENLABS_VOICE_ID must be alphanumeric", file=sys.stderr)
        sys.exit(1)
    return api_key, voice_id


def generate_audio_clips(
    segments: list[dict[str, str]],
    clips_dir: Path,
    name: str,
    api_key: str,
    voice_id: str,
) -> tuple[list[dict[str, float | str]], list[Path]]:
    """Generate audio for each segment and measure durations. Returns (timing, clip_paths)."""
    timing: list[dict[str, float | str]] = []
    clip_paths: list[Path] = []

    for seg in segments:
        clip_path = clips_dir / f"{name}_{seg['name']}.mp3"
        print(f"  Generating: {seg['name']} ({len(seg['text'])} chars)")
        generate_segment_audio(seg["text"], clip_path, api_key, voice_id)

        duration = get_audio_duration(clip_path)
        print(f"    Duration: {duration:.1f}s")

        timing.append({
            "name": seg["name"],
            "text": seg["text"],
            "duration": round(duration, 2),
            "clip": str(clip_path.name),
        })
        clip_paths.append(clip_path)

    return timing, clip_paths


def write_timing_and_summary(
    timing: list[dict[str, float | str]],
    clip_paths: list[Path],
    output_dir: Path,
    name: str,
) -> None:
    """Write timing JSON, concatenate narration track, and print the summary table."""
    timing_path = output_dir / f"{name}_timing.json"
    timing_path.write_text(json.dumps(timing, indent=2) + "\n")
    print(f"\nTiming written to {timing_path}")

    total = sum(float(t["duration"]) for t in timing)
    print(f"Total narration: {total:.1f}s")

    narration_path = output_dir / f"{name}-narration.mp3"
    concatenate_audio(clip_paths, narration_path)
    print(f"Combined narration: {narration_path}")

    print("\n--- Segment Timing (use for demo script sleep values) ---")
    for t in timing:
        text_preview = str(t["text"])[:60]
        print(f"  {t['name']:20s}  {float(t['duration']):5.1f}s  \"{text_preview}...\"")


def main() -> None:
    args = build_parser().parse_args()

    if not args.script.exists():
        print(f"Script not found: {args.script}", file=sys.stderr)
        sys.exit(1)

    segments = parse_segments(args.script)
    if not segments:
        print("No segments found in script", file=sys.stderr)
        sys.exit(1)
    print(f"Found {len(segments)} narration segments")

    warnings = lint_segments(segments)

    if args.dry_run:
        run_dry_run(segments, args.strict, warnings)
        return

    if args.strict and warnings:
        print(
            f"\n{warnings} contamination warning(s) in --strict mode. "
            "Fix the narration or rerun without --strict.",
            file=sys.stderr,
        )
        sys.exit(2)

    api_key, voice_id = validate_env()

    clips_dir = args.output_dir / "clips"
    clips_dir.mkdir(parents=True, exist_ok=True)

    timing, clip_paths = generate_audio_clips(
        segments, clips_dir, args.name, api_key, voice_id
    )

    write_timing_and_summary(timing, clip_paths, args.output_dir, args.name)

    if args.playwright:
        pw_path = args.output_dir / f"{args.name}-demo.spec.ts"
        generate_playwright_skeleton(timing, pw_path, args.name, args.base_url)
        print(f"\nPlaywright skeleton: {pw_path}")


if __name__ == "__main__":
    main()
