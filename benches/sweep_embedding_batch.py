"""Sweep llama-server -b/-ub on a throwaway port; never touches the managed service.

Measures, for each candidate (batch, ubatch):
  * whether a long single input (the "large file" path) is accepted or rejected
  * batch throughput at a few row counts
  * VRAM footprint, so the default can be bounded by real hardware

The managed service on :8080 is left alone; each candidate launches on --port.
"""

import argparse
import json
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path.home() / ".local/share/llama-ggmlorg/llama-b11574/llama-server"
MODEL = pathlib.Path.home() / ".leindex/models/embeddinggemma-2-Q8_0.gguf"

# Same text sizes the live probe used, so numbers are comparable.
LONG_TEXT = "fn process_changed_file(ctx: &mut Ctx) -> Result<()> { " * 3200  # ~176k chars
BATCH_TEXT = (
    "pub fn embed_batch_optional_titled(&self, texts: Vec<String>) -> "
    "Result<Vec<Vec<f32>>> { self.embed_batch(texts) } "
) * 4


def wait_ready(port, child, deadline_seconds=180):
    deadline = time.monotonic() + deadline_seconds
    while time.monotonic() < deadline:
        if child.poll() is not None:
            return False
        try:
            with urllib.request.urlopen(
                f"http://127.0.0.1:{port}/health", timeout=1
            ) as response:
                if response.status == 200:
                    return True
        except OSError:
            time.sleep(0.2)
    return False


def post(port, texts, timeout=300):
    body = json.dumps({"input": texts, "model": "embeddinggemma-2"}).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/embeddings",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.loads(response.read())
    return payload, time.perf_counter() - started


def run_candidate(batch, ubatch, device, port, rows):
    command = [
        str(BINARY),
        "-m", str(MODEL),
        "--host", "127.0.0.1", "--port", str(port),
        "--alias", "embeddinggemma-2", "--embedding", "--pooling", "mean",
        "--device", device, "-ngl", "99",
        "-b", str(batch), "-ub", str(ubatch),
        "-c", "262144", "-np", "4",
    ]
    label = f"b{batch}-ub{ubatch}"
    print(f"\n=== {label} (device {device})", flush=True)
    load_started = time.perf_counter()
    child = subprocess.Popen(
        command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT
    )
    try:
        if not wait_ready(port, child):
            print(f"  FAILED to become ready (exit {child.poll()})", flush=True)
            return
        print(f"  load_to_health={time.perf_counter() - load_started:.1f}s", flush=True)

        # Does the large-file path work at all at this batch size?
        try:
            payload, elapsed = post(port, [LONG_TEXT])
            dim = len(payload["data"][0]["embedding"])
            print(
                f"  long_input chars={len(LONG_TEXT)} tokens~57603 ACCEPTED "
                f"dim={dim} elapsed={elapsed:.2f}s",
                flush=True,
            )
        except urllib.error.HTTPError as error:
            detail = error.read()[:200].decode(errors="replace")
            print(f"  long_input REJECTED HTTP {error.code}: {detail}", flush=True)
        except Exception as error:  # noqa: BLE001
            print(f"  long_input {type(error).__name__}: {error}", flush=True)

        for size in rows:
            try:
                _, elapsed = post(port, [BATCH_TEXT] * size)
                print(
                    f"  rows={size:>4} elapsed={elapsed:>7.2f}s rows/s={size / elapsed:>7.2f}",
                    flush=True,
                )
            except urllib.error.HTTPError as error:
                detail = error.read()[:200].decode(errors="replace")
                print(f"  rows={size:>4} HTTP {error.code}: {detail}", flush=True)
            except Exception as error:  # noqa: BLE001
                print(f"  rows={size:>4} {type(error).__name__}: {error}", flush=True)
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=15)
        print(f"  reaped pid={child.pid} rc={child.returncode}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", default="Vulkan1")
    parser.add_argument("--port", type=int, default=8199)
    parser.add_argument(
        "--candidates",
        default="24576/24576,24576/512,65536/24576,65536/32768",
        help="comma-separated batch/ubatch pairs",
    )
    parser.add_argument("--rows", default="128,256,512")
    args = parser.parse_args()
    rows = [int(value) for value in args.rows.split(",")]

    for candidate in args.candidates.split(","):
        batch, ubatch = (int(part) for part in candidate.split("/"))
        run_candidate(batch, ubatch, args.device, args.port, rows)
    return 0


if __name__ == "__main__":
    sys.exit(main())
