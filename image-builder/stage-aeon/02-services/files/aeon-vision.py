#!/usr/bin/env python3
"""aeon-vision — on-device live OCR / object detection over the capture feed.

Pulls a frame from the running aeon-streamer (its unix socket /snapshot — so we
never open a second handle on the single-consumer CSI/USB capture device),
runs a vision model on it, and publishes structured detections to
/run/aeon/vision.json (atomic) for the supervisor's GET /api/vision/detections,
the MCP `screen_text` tool, and the AI agent. Boxes are emitted as **fractions
0..1** of the frame, the same coordinate convention the MCP click(x,y) tool
expects — so an agent can feed a detection box centre straight into a click.

Backends (set `backend` in /etc/aeon/vision.toml):
  * "tesseract" (default) — CPU OCR via the `tesseract` CLI (TSV word boxes).
    Works on any Pi out of the box; ideal for reading the controlled SCREEN.
  * "hailo" — OCR/detection on the Raspberry Pi AI HAT+ (Hailo accelerator).
    PLACEHOLDER: the HailoRT/PaddleOCR pipeline is finalised on-device once the
    HAT enumerates (the exact hailo-h10 runtime + .hef models are pinned then).
    Until wired, it logs and falls back to tesseract so the feature still works.

Self-disables (idles, writes {"present": false}) when vision is disabled in the
config, the streamer isn't serving frames, or the chosen backend's tooling is
absent — so the image is harmless when the feature is off or the HAT is missing.

Tunables (env, set in the unit, override vision.toml):
  AEON_VISION_INTERVAL_S (0.5)  — seconds between inferences (fps cap = 1/this)
  AEON_VISION_BACKEND    (tesseract|hailo)
  AEON_VISION_MIN_CONF   (40)   — drop detections below this confidence (0-100)
  AEON_VISION_LANG       (eng)  — tesseract language
"""
import json
import os
import subprocess
import sys
import time

OUT = "/run/aeon/vision.json"
STREAMER_SOCK = os.environ.get("AEON_STREAMER_SOCK", "/run/aeon/streamer.sock")
VISION_TOML = "/etc/aeon/vision.toml"
STREAMER_TOML = "/etc/aeon/streamer.toml"
FRAME_TMP = "/run/aeon/.vision-frame.jpg"

# Hailo OCR (PaddleOCR PP-OCRv5 on the AI HAT+): det + rec .hef + the rec char
# dictionary, dropped by `aeon-hailo deploy paddle_ocr_v5_mobile_{detection,
# recognition}` + the ppocrv5 dict. ocr_hailo() loads them lazily; if any is
# absent it raises and main() falls back to tesseract, so OCR keeps working.
HEF_DIR = "/usr/share/aeon/hailo/hef"
DET_HEF = HEF_DIR + "/paddle_ocr_v5_mobile_detection.hef"
REC_HEF = HEF_DIR + "/paddle_ocr_v5_mobile_recognition.hef"
OCR_DICT = HEF_DIR + "/ppocrv5_dict.txt"
DET_W, DET_H, REC_W, REC_H = 960, 544, 320, 48


def load_cfg():
    """vision.toml → dict, with env overrides. Stdlib tomllib (Py 3.11+)."""
    cfg = {}
    try:
        import tomllib
        with open(VISION_TOML, "rb") as f:
            cfg = tomllib.load(f)
    except (OSError, ImportError, ValueError):
        cfg = {}
    enabled = bool(cfg.get("enabled", False))
    interval = float(os.environ.get("AEON_VISION_INTERVAL_S", cfg.get("interval_s", 0.5)))
    backend = os.environ.get("AEON_VISION_BACKEND", cfg.get("backend", "tesseract"))
    min_conf = int(os.environ.get("AEON_VISION_MIN_CONF", cfg.get("min_conf", 40)))
    lang = os.environ.get("AEON_VISION_LANG", cfg.get("lang", "eng"))
    max_det = int(cfg.get("max_detections", 200))
    return {
        "enabled": enabled, "interval": max(0.1, interval), "backend": backend,
        "min_conf": min_conf, "lang": lang, "max_det": max_det,
    }


def view_source():
    """What the streamer is currently showing — tags detections for context."""
    try:
        import tomllib
        with open(STREAMER_TOML, "rb") as f:
            return tomllib.load(f).get("capture", {}).get("source", "auto")
    except Exception:
        return "auto"


def write_json(obj):
    tmp = OUT + ".tmp"
    try:
        with open(tmp, "w") as f:
            json.dump(obj, f)
        os.replace(tmp, OUT)
    except OSError as e:
        sys.stderr.write(f"aeon-vision: write {OUT}: {e}\n")


def grab_frame():
    """Pull one JPEG from the streamer's unix socket. Returns bytes or None.

    Hits /snapshot on /run/aeon/streamer.sock directly (aeon-owned 0660; we run
    as root) so we don't fight the capture device for a second open.
    """
    try:
        out = subprocess.run(
            ["curl", "-s", "--max-time", "4", "--unix-socket", STREAMER_SOCK,
             "http://localhost/snapshot"],
            capture_output=True, timeout=6,
        )
    except (subprocess.SubprocessError, OSError) as e:
        sys.stderr.write(f"aeon-vision: streamer grab failed: {e}\n")
        return None
    data = out.stdout
    # A real JPEG starts FF D8; anything else (HTML error, empty) → no frame.
    if len(data) > 3 and data[0] == 0xFF and data[1] == 0xD8:
        return data
    return None


def jpeg_size(data):
    """(w, h) from the JPEG SOFn marker, no PIL. Returns (0, 0) if unparseable."""
    i, n = 2, len(data)
    while i < n - 9:
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            h = (data[i + 5] << 8) | data[i + 6]
            w = (data[i + 7] << 8) | data[i + 8]
            return w, h
        if i + 3 >= n:
            break
        seg = (data[i + 2] << 8) | data[i + 3]
        i += 2 + seg
    return 0, 0


def ocr_tesseract(jpeg, w, h, lang, min_conf):
    """CPU OCR via `tesseract … tsv`. Returns (detections, kind)."""
    try:
        with open(FRAME_TMP, "wb") as f:
            f.write(jpeg)
        out = subprocess.run(
            ["tesseract", FRAME_TMP, "stdout", "-l", lang, "--psm", "11", "tsv"],
            capture_output=True, timeout=20, text=True,
        )
    except FileNotFoundError:
        raise RuntimeError("tesseract not installed")
    except (subprocess.SubprocessError, OSError) as e:
        raise RuntimeError(f"tesseract run failed: {e}")

    dets = []
    lines = out.stdout.splitlines()
    if not lines:
        return dets, "text"
    # header: level page block par line word left top width height conf text
    for row in lines[1:]:
        c = row.split("\t")
        if len(c) < 12:
            continue
        text = c[11].strip()
        if not text:
            continue
        try:
            conf = float(c[10])
            left, top, bw, bh = int(c[6]), int(c[7]), int(c[8]), int(c[9])
        except ValueError:
            continue
        if conf < min_conf or w <= 0 or h <= 0:
            continue
        dets.append({
            "text": text,
            "conf": round(conf / 100.0, 3),
            "box": {"x": round(left / w, 4), "y": round(top / h, 4),
                    "w": round(bw / w, 4), "h": round(bh / h, 4)},
            "box_px": {"x": left, "y": top, "w": bw, "h": bh},
        })
    return dets, "text"


# ── Hailo PaddleOCR engine cache (built once, reused for the daemon's life) ──
_OCR = {"vdev": None, "det": None, "rec": None, "chars": None}


def _ocr_load_chars():
    """PP-OCRv5 CTC label table: ['blank'] + dict chars + [' '] → 18385 classes."""
    lines = [l.rstrip("\n") for l in open(OCR_DICT, encoding="utf-8")]
    return ["<blank>"] + lines + [" "]


def _ocr_engines():
    """Lazily build + cache the det + rec InferModels (HailoRT 5 InferModel API).
    Raises if pyhailort / the .hef / the dict are missing → tesseract fallback."""
    if _OCR["det"] is not None:
        return _OCR
    import numpy  # noqa: F401  (must be present before we commit to this path)
    from hailo_platform import VDevice, FormatType
    for p in (DET_HEF, REC_HEF, OCR_DICT):
        if not os.path.isfile(p):
            raise RuntimeError(f"hailo OCR asset missing: {p}")
    vdev = VDevice()

    def build(hef):
        im = vdev.create_infer_model(hef)
        im.set_batch_size(1)
        im.input().set_format_type(FormatType.UINT8)
        im.output().set_format_type(FormatType.FLOAT32)
        return {"cim": im.configure(), "out_shape": tuple(im.output().shape)}

    _OCR["vdev"] = vdev
    _OCR["det"] = build(DET_HEF)
    _OCR["rec"] = build(REC_HEF)
    _OCR["chars"] = _ocr_load_chars()
    return _OCR


def _ocr_infer(eng, inp_uint8):
    """One InferModel run; returns the output array (copied — buffer is reused)."""
    import numpy as np
    cim = eng["cim"]
    b = cim.create_bindings()
    b.input().set_buffer(np.ascontiguousarray(inp_uint8, dtype=np.uint8))
    out = np.zeros(eng["out_shape"], dtype=np.float32)
    b.output().set_buffer(out)
    cim.run([b], 10000)
    return np.asarray(b.output().get_buffer()).copy()


def _rec_preprocess(crop_rgb):
    """PaddleOCR rec preprocess: resize to height 48 keeping aspect, pad to 320.
    (A hard stretch distorts glyphs — aspect-preserving + pad is what reads right.)"""
    import numpy as np
    import cv2
    ih, iw = crop_rgb.shape[:2]
    rw = min(REC_W, max(1, int(np.ceil(REC_H * iw / max(1, ih)))))
    out = np.zeros((REC_H, REC_W, 3), dtype=np.uint8)
    out[:, :rw] = cv2.resize(crop_rgb, (rw, REC_H))
    return out


def _ctc_decode(logits, chars):
    """CTC greedy decode (collapse repeats, drop blank=0). Returns (text, mean_conf)."""
    import numpy as np
    arr = np.asarray(logits).reshape(-1, len(chars))
    ids = arr.argmax(-1)
    prev, out, conf = -1, [], []
    for t, i in enumerate(ids):
        if i != prev and i != 0 and i < len(chars):
            out.append(chars[i])
            conf.append(float(arr[t, i]))
        prev = i
    return "".join(out), (float(np.mean(conf)) if conf else 0.0)


def ocr_hailo(jpeg, w, h, lang, min_conf):
    """On-device OCR via the Hailo AI HAT+ — PaddleOCR PP-OCRv5 (DB text-detect →
    CTC recognise). Returns (detections, "text") in the SAME shape as
    ocr_tesseract; raises (→ tesseract fallback) if the runtime/models are absent.
    Validated on the Hailo-10H: ~0.5s detect + ~30ms/box recognise."""
    import numpy as np
    import cv2
    eng = _ocr_engines()
    chars = eng["chars"]

    img = cv2.imdecode(np.frombuffer(jpeg, np.uint8), cv2.IMREAD_COLOR)  # BGR
    if img is None:
        raise RuntimeError("hailo OCR: undecodable JPEG")
    ih, iw = img.shape[:2]

    # ── detection (DB) → probability map → text boxes ──
    # Feed BGR (cv2's native order) to match the reference PaddleOCR pipeline —
    # RGB mis-reads COLORED text (validated: red-on-white drops digits under RGB).
    det_in = cv2.resize(img, (DET_W, DET_H)).astype(np.uint8)
    prob = _ocr_infer(eng["det"], det_in).reshape(DET_H, DET_W)
    binmap = (prob > 0.3).astype(np.uint8) * 255
    cnts, _ = cv2.findContours(binmap, cv2.RETR_EXTERNAL, cv2.CHAIN_APPROX_SIMPLE)
    sx, sy = iw / DET_W, ih / DET_H

    boxes = []
    for c in cnts:
        if cv2.contourArea(c) < 16:
            continue
        x, y, bw, bh = cv2.boundingRect(c)
        if prob[y:y + bh, x:x + bw].mean() < 0.5:
            continue
        # simple unclip (expand the box) — avoids a pyclipper dependency.
        pad = int(0.18 * min(bw, bh)) + 2
        boxes.append((max(0, x - pad), max(0, y - pad),
                      min(DET_W, x + bw + pad), min(DET_H, y + bh + pad)))
    boxes.sort(key=lambda b: (b[1], b[0]))  # reading order: top→bottom, left→right

    # ── recognition per box (CTC) ──
    dets = []
    for (x0, y0, x1, y1) in boxes:
        ox0, oy0 = int(x0 * sx), int(y0 * sy)
        ox1, oy1 = int(x1 * sx), int(y1 * sy)
        crop = img[oy0:oy1, ox0:ox1]
        if crop.size == 0:
            continue
        text, conf = _ctc_decode(
            _ocr_infer(eng["rec"], _rec_preprocess(crop)),  # BGR (cv2 native — see above)
            chars,
        )
        text = text.strip()
        if not text or conf * 100 < min_conf or iw <= 0 or ih <= 0:
            continue
        bw, bh = ox1 - ox0, oy1 - oy0
        dets.append({
            "text": text,
            "conf": round(conf, 3),
            "box": {"x": round(ox0 / iw, 4), "y": round(oy0 / ih, 4),
                    "w": round(bw / iw, 4), "h": round(bh / ih, 4)},
            "box_px": {"x": ox0, "y": oy0, "w": bw, "h": bh},
        })
    return dets, "text"


def main():
    cfg = load_cfg()
    if not cfg["enabled"]:
        write_json({"present": False})
        sys.stderr.write("aeon-vision: disabled in vision.toml; idling\n")
        # Re-read config periodically so a PUT that flips it on takes effect
        # without a manual restart (the supervisor restarts us on config write,
        # but idle-and-recheck is a harmless safety net).
        while not cfg["enabled"]:
            time.sleep(30)
            cfg = load_cfg()

    backend = cfg["backend"]
    sys.stderr.write(f"aeon-vision: starting (backend={backend}, "
                     f"interval={cfg['interval']}s)\n")
    consec_noframe = 0

    while True:
        cfg = load_cfg()
        if not cfg["enabled"]:
            write_json({"present": False})
            time.sleep(2)
            continue

        jpeg = grab_frame()
        if jpeg is None:
            consec_noframe += 1
            # Streamer down / no source → report not-present, back off a bit.
            write_json({"present": False, "ok": False,
                        "error": "no frame from streamer"})
            time.sleep(min(5, 1 + consec_noframe))
            continue
        consec_noframe = 0

        w, h = jpeg_size(jpeg)
        t0 = time.monotonic()
        try:
            if backend == "hailo":
                try:
                    dets, kind = ocr_hailo(jpeg, w, h, cfg["lang"], cfg["min_conf"])
                except RuntimeError as e:
                    sys.stderr.write(f"aeon-vision: {e}; falling back to tesseract\n")
                    dets, kind = ocr_tesseract(jpeg, w, h, cfg["lang"], cfg["min_conf"])
            else:
                dets, kind = ocr_tesseract(jpeg, w, h, cfg["lang"], cfg["min_conf"])
        except RuntimeError as e:
            write_json({"present": False, "ok": False, "error": str(e)})
            sys.stderr.write(f"aeon-vision: {e}; idling\n")
            time.sleep(30)
            continue
        infer_ms = int((time.monotonic() - t0) * 1000)

        if len(dets) > cfg["max_det"]:
            dets = sorted(dets, key=lambda d: d.get("conf", 0), reverse=True)[:cfg["max_det"]]

        write_json({
            "present": True, "ok": True,
            "source": view_source(), "backend": backend, "kind": kind,
            "frame": {"w": w, "h": h},
            "ts_ms": int(time.time() * 1000), "infer_ms": infer_ms,
            "count": len(dets),
            "detections": dets,
        })
        time.sleep(cfg["interval"])


if __name__ == "__main__":
    main()
