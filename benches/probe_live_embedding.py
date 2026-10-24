"""Probe the live managed embedding server: throughput, and what input sizes it accepts.

No new server is launched; this measures whatever llama-server leindex is managing
on 127.0.0.1:8080 right now, so the numbers reflect the real configured launch.
"""

import json
import statistics
import sys
import time
import urllib.error
import urllib.request

URL = "http://127.0.0.1:8080/v1/embeddings"


def post(texts, timeout=300):
    body = json.dumps({"input": texts, "model": "embeddinggemma-2"}).encode()
    request = urllib.request.Request(
        URL, data=body, headers={"Content-Type": "application/json"}
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=timeout) as response:
        payload = json.loads(response.read())
    return payload, time.perf_counter() - started


def main():
    # A single long input: this is the large-file path. If the server rejects it,
    # that is the "large file rejection" symptom.
    for repeat in (200, 800, 3200):
        text = "fn process_changed_file(ctx: &mut Ctx) -> Result<()> { " * repeat
        try:
            payload, elapsed = post([text])
            dim = len(payload["data"][0]["embedding"])
            print(
                f"single_input chars={len(text):>8} dim={dim} ok elapsed={elapsed:.2f}s",
                flush=True,
            )
        except urllib.error.HTTPError as error:
            detail = error.read()[:300].decode(errors="replace")
            print(
                f"single_input chars={len(text):>8} HTTP {error.code}: {detail}",
                flush=True,
            )
        except Exception as error:  # noqa: BLE001 - probe must report, not raise
            print(f"single_input chars={len(text):>8} {type(error).__name__}: {error}", flush=True)

    # Batch throughput: how does wall time scale with batch size at fixed text size?
    base = (
        "pub fn embed_batch_optional_titled(&self, texts: Vec<String>) -> "
        "Result<Vec<Vec<f32>>> { self.embed_batch(texts) } "
    )
    print("\nbatch throughput (same 4x-repeated text per row):", flush=True)
    for size in (16, 64, 128, 256, 512):
        texts = [base * 4] * size
        try:
            _, elapsed = post(texts)
            rates = size / elapsed
            print(
                f"  rows={size:>4} elapsed={elapsed:>7.2f}s rows/s={rates:>7.2f}",
                flush=True,
            )
        except urllib.error.HTTPError as error:
            detail = error.read()[:300].decode(errors="replace")
            print(f"  rows={size:>4} HTTP {error.code}: {detail}", flush=True)
        except Exception as error:  # noqa: BLE001
            print(f"  rows={size:>4} {type(error).__name__}: {error}", flush=True)

    # Concurrency: the server has -np slots; does parallel submission help?
    print("\nrepeatability at rows=128:", flush=True)
    times = []
    for _ in range(3):
        try:
            _, elapsed = post([base * 4] * 128)
            times.append(elapsed)
        except Exception as error:  # noqa: BLE001
            print(f"  {type(error).__name__}: {error}", flush=True)
            return
    print(
        f"  min={min(times):.2f}s median={statistics.median(times):.2f}s "
        f"max={max(times):.2f}s",
        flush=True,
    )


if __name__ == "__main__":
    sys.exit(main())
