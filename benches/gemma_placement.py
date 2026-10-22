"""Isolated EmbeddingGemma2 placement/batch experiment; never touches :8080.

Run after `cargo +stable test --release --bench neural_stage --no-run`:
    python benches/gemma_placement.py --device Vulkan1 --ubatch 512
The temporary server is always terminated/reaped; logs remain under /tmp.
"""

import argparse
import os
import pathlib
import subprocess
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    # This machine enumerates the Ryzen iGPU as Vulkan0. Never benchmark it.
    parser.add_argument(
        "--device", choices=["Vulkan1", "Vulkan2", "Vulkan1,Vulkan2"], default="Vulkan1"
    )
    parser.add_argument("--ubatch", type=int, default=512)
    parser.add_argument("--batch", type=int, default=2048)
    parser.add_argument("--rows", type=int, default=1024)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--batches", default="32,256,512")
    parser.add_argument("--workers", default="1,2")
    parser.add_argument("--text-repeats", type=int, default=4)
    parser.add_argument("--port", type=int, default=8081)
    parser.add_argument("--cpu", action="store_true")
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    binaries = list((root / "target/release/deps").glob("neural_stage-*"))
    binaries = [p for p in binaries if p.is_file() and os.access(p, os.X_OK)]
    if len(binaries) != 1:
        raise RuntimeError(f"expected one built neural_stage executable, got {binaries}")
    label = f"{args.device}-ub{args.ubatch}-b{args.batch}-cpu{args.cpu}-{os.getpid()}"
    server_log = pathlib.Path(f"/tmp/gemma-placement-{label}.log")
    command = [
        str(pathlib.Path.home() / ".local/share/llama-ggmlorg/llama-b11574/llama-server"),
        "-m", str(pathlib.Path.home() / ".leindex/models/embeddinggemma-2-Q8_0.gguf"),
        "--host", "127.0.0.1", "--port", str(args.port),
        "--alias", "embeddinggemma-2", "--embedding", "--pooling", "mean",
        "-ngl", "0" if args.cpu else "99", "--device", args.device,
        "-b", str(args.batch), "-ub", str(args.ubatch), "-c", "262144", "-np", "4",
    ]
    print("server_command", command, "log", str(server_log), flush=True)
    start = time.perf_counter()
    with server_log.open("xb") as log:
        child = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
        try:
            deadline = time.monotonic() + 120
            while True:
                if child.poll() is not None:
                    raise RuntimeError(f"server exited {child.returncode}; inspect {server_log}")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{args.port}/health", timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise TimeoutError("server readiness deadline exceeded")
                    time.sleep(0.1)
            print("load_to_health_seconds", time.perf_counter() - start, flush=True)
            env = os.environ.copy()
            env.update({
                "LEINDEX_PERF_BACKEND": "http", "LEINDEX_PERF_NO_POOL": "1",
                "LEINDEX_PERF_URL": f"http://127.0.0.1:{args.port}/v1/embeddings",
                "LEINDEX_PERF_ROWS": str(args.rows), "LEINDEX_PERF_REPEATS": str(args.repeats),
                "LEINDEX_PERF_BATCHES": args.batches, "LEINDEX_PERF_WORKERS": args.workers,
                "LEINDEX_PERF_TEXT_REPEATS": str(args.text_repeats),
            })
            subprocess.run([str(binaries[0]), "--ignored", "--nocapture"], env=env,
                           check=True, timeout=600, cwd=root)
        finally:
            if child.poll() is None:
                child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=10)
            print("temporary_server_reaped", child.pid, child.returncode, flush=True)


if __name__ == "__main__":
    main()
