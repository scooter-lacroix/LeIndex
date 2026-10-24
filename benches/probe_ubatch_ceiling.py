"""Find the -ub that covers the largest document the client is allowed to send.

Invariant under test: the client's per-text ceiling is `max_seq_len: 64 * 1024`
bytes (neural_publish.rs). Code tokenizes near 2.4 bytes/token, so a legal
65 KiB text is ~27-33k tokens. Any -ub below that MUST reject it, and a
rejection is an HTTP 500 that burns the caller's failure budget.

Each candidate runs on its own port with stderr captured, so an OOM shows up
as a logged allocator failure instead of a silent stall.
"""

import argparse
import json
import pathlib
import subprocess
import sys
import time
import urllib.error
import urllib.request

BINARY = pathlib.Path.home() / ".local/share/llama-ggmlorg/llama-b11574/llama-server"
MODEL = pathlib.Path.home() / ".leindex/models/embeddinggemma-2-Q8_0.gguf"

# The largest text the client may legally send, built from real-looking Rust so
# the tokenizer sees realistic punctuation density rather than prose.
UNIT = "pub fn process_changed_file(ctx: &mut BatchCtx<'_>) -> Result<()> { self.emit(ctx)?; }\n"
LONG = (UNIT * 800)[: 64 * 1024]

BATCH_TEXT = "pub fn embed_batch(&self, t: &[String]) -> Result<Vec<Vec<f32>>> { Ok(vec![]) } // handled\n"


def wait_ready(port, child, budget=240):
    deadline = time.monotonic() + budget
    while time.monotonic() < deadline:
        if child.poll() is not None:
            return False
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=1) as r:
                if r.status == 200:
                    return True
        except OSError:
            time.sleep(0.25)
    return False


def post(port, texts, timeout=600):
    body = json.dumps({"input": texts, "model": "embeddinggemma-2"}).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/v1/embeddings",
        data=body,
        headers={"Content-Type": "application/json"},
    )
    started = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read()), time.perf_counter() - started


def run(batch, ubatch, device, port, rows):
    log = pathlib.Path(f"/tmp/ub-probe-{ubatch}.log")
    cmd = [
        str(BINARY), "-m", str(MODEL),
        "--host", "127.0.0.1", "--port", str(port),
        "--alias", "embeddinggemma-2", "--embedding", "--pooling", "mean",
        "--device", device, "-ngl", "99",
        "-b", str(batch), "-ub", str(ubatch),
        "-c", "262144", "-np", "4",
    ]
    print(f"\n=== -b {batch} -ub {ubatch}  (log {log})", flush=True)
    t0 = time.perf_counter()
    with log.open("wb") as handle:
        child = subprocess.Popen(cmd, stdout=handle, stderr=subprocess.STDOUT)
    try:
        if not wait_ready(port, child):
            print(f"  NOT READY (exit {child.poll()}) - see {log}", flush=True)
            return
        print(f"  ready in {time.perf_counter() - t0:.1f}s", flush=True)

        t0 = time.perf_counter()
        try:
            payload, elapsed = post(port, [LONG])
            dim = len(payload["data"][0]["embedding"])
            print(
                f"  64KiB text: ACCEPTED dim={dim} in {elapsed:.2f}s "
                f"(total {time.perf_counter() - t0:.1f}s)",
                flush=True,
            )
        except urllib.error.HTTPError as e:
            print(f"  64KiB text: REJECTED HTTP {e.code} {e.read()[:160].decode(errors='replace')}", flush=True)
        except Exception as e:  # noqa: BLE001
            print(f"  64KiB text: {type(e).__name__}: {e}", flush=True)

        for size in rows:
            try:
                _, elapsed = post(port, [BATCH_TEXT] * size)
                print(
                    f"  rows={size:>4} {elapsed:>6.2f}s  {size / elapsed:>7.2f} rows/s",
                    flush=True,
                )
            except urllib.error.HTTPError as e:
                print(f"  rows={size:>4} HTTP {e.code}: {e.read()[:120].decode(errors='replace')}", flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"  rows={size:>4} {type(e).__name__}: {e}", flush=True)
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=20)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=20)
        oom = sum(
            1
            for line in log.read_text(errors="replace").splitlines()
            if "ErrorOutOfDeviceMemory" in line or "failed to allocate" in line.lower()
        )
        print(f"  reaped rc={child.returncode} oom_lines={oom}", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--device", default="Vulkan1")
    ap.add_argument("--port", type=int, default=8199)
    ap.add_argument("--candidates", default="32768,49152,65536")
    ap.add_argument("--batch", type=int, default=65536)
    ap.add_argument("--rows", default="128,256")
    args = ap.parse_args()
    rows = [int(v) for v in args.rows.split(",")]
    for ub in (int(v) for v in args.candidates.split(",")):
        run(args.batch, ub, args.device, args.port, rows)
    return 0


if __name__ == "__main__":
    sys.exit(main())
