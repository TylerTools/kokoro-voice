#!/usr/bin/env python3
"""
Screen-snip OCR — drag a box around anything on screen and read it aloud.

For text you cannot select: images, PDFs in a viewer, video frames, remote
desktops, screenshots someone sent you.

    snip.py                 select a region, print the text
    snip.py --speak         select a region, read it aloud
    snip.py --file shot.png OCR an existing image instead of capturing

Uses Apple Vision on macOS and Windows.Media.Ocr on Windows. Both ship with the OS; the
OCR is on-device, needs no model download, and makes no network calls, so the
fully-local guarantee holds. Measured on a 1000x340 text image: 341ms at
`accurate` with confidence 1.00, 22ms at `fast` for identical output.

REQUIRES the Screen Recording permission (System Settings -> Privacy &
Security -> Screen Recording) for whichever app launches this. Without it
`screencapture` fails with "could not create image from display" rather than
anything more helpful.
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
import time

IS_MAC = sys.platform == "darwin"
IS_WIN = sys.platform == "win32"


def _state_dir() -> str:
    """Private 0700 scratch dir — same rationale as speak.py.

    A snip can contain anything on screen: passwords, medical records, a
    private message. It must not land in a directory other users can read, and
    it is deleted as soon as the text is extracted.
    """
    override = os.environ.get("KOKORO_STATE_DIR")
    if override:
        base = override
    else:
        try:
            who = str(os.getuid())
        except AttributeError:  # Windows
            who = os.environ.get("USERNAME", "user")
        base = os.path.join(tempfile.gettempdir(), f"kokoro-voice-2-1-{who}")
    os.makedirs(base, mode=0o700, exist_ok=True)
    if hasattr(os, "getuid"):
        st = os.lstat(base)
        if os.path.islink(base) or st.st_uid != os.getuid():
            raise SystemExit(f"refusing to use {base}: not a directory we own")
        os.chmod(base, 0o700)
    return base


def capture_region(path: str) -> bool:
    """Interactive crosshair selection. False if the user cancelled.

    -i interactive, -x silent (no shutter sound). screencapture simply writes
    no file when you press Escape, which is how cancellation is detected.
    """
    if IS_MAC:
        subprocess.run(["screencapture", "-i", "-x", path], check=False)
    elif IS_WIN:
        script = r'''
Add-Type -AssemblyName System.Windows.Forms
[Windows.Forms.Clipboard]::Clear()
Start-Process 'ms-screenclip:'
$deadline = (Get-Date).AddSeconds(120)
while ((Get-Date) -lt $deadline) {
  Start-Sleep -Milliseconds 200
  if ([Windows.Forms.Clipboard]::ContainsImage()) {
    $image = [Windows.Forms.Clipboard]::GetImage()
    $image.Save($env:KOKORO_SNIP_PATH, [Drawing.Imaging.ImageFormat]::Png)
    exit 0
  }
}
exit 1
'''
        env = os.environ.copy()
        env["KOKORO_SNIP_PATH"] = os.path.abspath(path)
        subprocess.run(["powershell", "-NoProfile", "-STA", "-Command", script], env=env, check=False)
    else:
        return False
    return os.path.exists(path) and os.path.getsize(path) > 0


def ocr_image(path: str, accurate: bool = True, languages=("en-US",)) -> str:
    """Run Vision OCR and return the text in reading order."""
    if IS_WIN:
        script = r'''
Add-Type -AssemblyName System.Runtime.WindowsRuntime
function Await-Result($operation, [Type]$resultType) {
  $method = [System.WindowsRuntimeSystemExtensions].GetMethods() |
    Where-Object { $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and $_.GetParameters().Count -eq 1 } |
    Select-Object -First 1
  $task = $method.MakeGenericMethod($resultType).Invoke($null, @($operation))
  $task.Wait()
  return $task.Result
}
[Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime] | Out-Null
[Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics.Imaging, ContentType=WindowsRuntime] | Out-Null
[Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType=WindowsRuntime] | Out-Null
$file = Await-Result ([Windows.Storage.StorageFile]::GetFileFromPathAsync($env:KOKORO_OCR_PATH)) ([Windows.Storage.StorageFile])
$stream = Await-Result ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStreamWithContentType])
$decoder = Await-Result ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
$bitmap = Await-Result ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($null -eq $engine) { throw 'No Windows OCR language is installed.' }
$result = Await-Result ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$result.Text
'''
        env = os.environ.copy()
        env["KOKORO_OCR_PATH"] = os.path.abspath(path)
        out = subprocess.run(
            ["powershell", "-NoProfile", "-STA", "-Command", script],
            env=env, capture_output=True, text=True,
        )
        if out.returncode != 0:
            raise RuntimeError(out.stderr.strip() or "Windows OCR failed")
        return out.stdout.strip()

    import Quartz
    import Vision
    from Foundation import NSURL

    src = Quartz.CGImageSourceCreateWithURL(NSURL.fileURLWithPath_(path), None)
    if src is None:
        raise RuntimeError(f"could not read image: {path}")
    image = Quartz.CGImageSourceCreateImageAtIndex(src, 0, None)
    if image is None or Quartz.CGImageGetWidth(image) == 0:
        raise RuntimeError(f"could not decode image: {path}")

    # NOTE: build the request with plain init() and read results() after
    # performRequests_error_ returns. The initWithCompletionHandler_ form looks
    # natural from the Swift docs but silently yields zero observations here.
    request = Vision.VNRecognizeTextRequest.alloc().init()
    request.setRecognitionLevel_(0 if accurate else 1)  # 0 = accurate, 1 = fast
    request.setUsesLanguageCorrection_(True)
    request.setRecognitionLanguages_(list(languages))

    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(image, None)
    ok, err = handler.performRequests_error_([request], None)
    if not ok:
        raise RuntimeError(f"OCR failed: {err}")

    # Vision returns observations in no guaranteed order, with normalized
    # boxes whose origin is BOTTOM-left. Sort top-to-bottom then left-to-right,
    # grouping into rows with a tolerance so that words sitting side by side on
    # the same visual line don't get interleaved with the line below.
    items = []
    for obs in request.results() or []:
        cand = obs.topCandidates_(1)
        if not cand:
            continue
        box = obs.boundingBox()
        items.append({
            "text": cand[0].string(),
            "conf": float(cand[0].confidence()),
            "top": 1.0 - (box.origin.y + box.size.height),
            "left": box.origin.x,
            "height": box.size.height,
        })
    if not items:
        return ""

    tol = max(sum(i["height"] for i in items) / len(items) * 0.6, 0.005)
    items.sort(key=lambda i: (i["top"], i["left"]))
    lines, row, baseline = [], [], items[0]["top"]
    for it in items:
        if abs(it["top"] - baseline) <= tol:
            row.append(it)
        else:
            lines.append(sorted(row, key=lambda i: i["left"]))
            row, baseline = [it], it["top"]
    lines.append(sorted(row, key=lambda i: i["left"]))

    return "\n".join(" ".join(i["text"] for i in line) for line in lines).strip()


def main() -> int:
    ap = argparse.ArgumentParser(description="Screen-snip OCR")
    ap.add_argument("--speak", action="store_true", help="read the text aloud")
    ap.add_argument("--file", help="OCR this image instead of capturing")
    ap.add_argument("--fast", action="store_true",
                    help="fast recognition (~15x quicker, slightly less robust)")
    ap.add_argument("--lang", default="en-US", help="recognition language(s), comma separated")
    args = ap.parse_args()

    if not (IS_MAC or IS_WIN) and not args.file:
        print("ERROR screen capture is unsupported on this platform", file=sys.stderr)
        return 2

    path, temporary = args.file, False
    if not path:
        path = os.path.join(_state_dir(), f"snip-{os.getpid()}.png")
        temporary = True
        if not capture_region(path):
            print("CANCELLED", flush=True)          # Escape pressed — not an error
            return 1

    try:
        t0 = time.time()
        text = ocr_image(path, accurate=not args.fast, languages=args.lang.split(","))
        elapsed = time.time() - t0
    except Exception as e:  # noqa: BLE001
        print(f"ERROR {e}", file=sys.stderr)
        return 2
    finally:
        # Delete the capture whatever happened. It may contain anything that was
        # on screen, and nothing downstream needs the pixels.
        if temporary:
            try:
                os.remove(path)
            except OSError:
                pass

    if not text:
        print("ERROR no text found in that region", flush=True)
        return 1

    print(f"OCR {len(text)} chars in {elapsed:.2f}s", file=sys.stderr)

    if args.speak:
        here = os.path.dirname(os.path.abspath(__file__))
        return subprocess.run(
            [sys.executable, os.path.join(here, "speak.py"), "--stdin"],
            input=text, text=True,
        ).returncode

    print(text, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
