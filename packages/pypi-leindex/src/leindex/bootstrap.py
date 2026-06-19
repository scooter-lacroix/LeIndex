"""PyPI bootstrap wrapper for the Rust LeIndex binary.

This module is the implementation behind the ``leindex`` and ``leindex-setup``
console scripts declared in ``pyproject.toml``. The wrapper ensures the Rust
``leindex`` binary (and its ``leindex-embed`` worker sibling) is installed via
``cargo install``, then forwards CLI execution to the real binary while
relaying signals and exit codes.

Design notes (see ``validation-contract.md`` Area: PYPI):

* VAL-PYPI-002/003: bootstrap installs or upgrades the cargo binary to the
  matching wrapper version.
* VAL-PYPI-004: after a fresh bootstrap, the wrapper offers/runs
  ``leindex setup`` interactively (TTY) or prints deferrable guidance when
  stdout is not a TTY.
* VAL-PYPI-005: a dedicated ``setup_main`` entry point backs the
  ``leindex-setup`` console script that always runs the setup wizard.
* VAL-PYPI-007: signals (SIGINT/SIGTERM) propagate to the child and the
  child's real exit code is returned (no KeyboardInterrupt traceback).
* VAL-PYPI-008: the worker binary (``leindex-embed``) is bootstrapped in the
  same pass so neural search is functional after setup.
* VAL-PYPI-011: a missing ``cargo`` is handled with actionable guidance and a
  non-zero exit instead of a hang or silent failure.
"""

from __future__ import annotations

import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterable, Sequence

from . import __version__

REPO_URL = "https://github.com/scooter-lacroix/LeIndex"
RUSTUP_UNIX_URL = "https://sh.rustup.rs"
RUSTUP_WINDOWS_URL = "https://win.rustup.rs/x86_64"
ENV_VERSION_OVERRIDE = "LEINDEX_RUST_VERSION"
ENV_SKIP_BOOTSTRAP = "LEINDEX_SKIP_PYPI_BOOTSTRAP"
# Extra used during ``leindex setup``-after-bootstrap and by tests to force a
# deterministic wanted version regardless of the published ``__version__``.
ENV_SKIP_SETUP_AFTER_BOOTSTRAP = "LEINDEX_SKIP_SETUP_AFTER_BOOTSTRAP"
INSTALL_ONLY_FLAG = "--bootstrap-install-only"

# Feature flag set used when bootstrapping the Rust binaries via cargo install.
# ``onnx`` enables the load-dynamic ORT bindings + worker wiring so neural
# search is available after ``leindex setup`` (no ORT is needed at build time
# under load-dynamic, so the install succeeds on a clean host).
INSTALL_FEATURES = "onnx"

# Main binary package name on crates.io.
MAIN_PACKAGE = "leindex"
# Worker binary package name on crates.io (separate package that ships the
# ``leindex-embed`` executable). ``cargo install leindex`` only installs the
# main crate's bin targets, so the worker must be installed explicitly.
WORKER_PACKAGE = "leindex-embed"


class BootstrapError(RuntimeError):
    """Raised when the PyPI bootstrapper cannot provision LeIndex."""


@dataclass(frozen=True)
class InstallTarget:
    cargo_home: Path
    cargo_bin: Path
    cargo_binary: Path
    leindex_binary: Path
    # VAL-PYPI-008: the worker binary is a separate package with its own
    # install target. Both live under the same ``$CARGO_HOME/bin`` directory.
    embed_binary: Path


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    install_only = False

    if args and args[0] == INSTALL_ONLY_FLAG:
        install_only = True
        args = args[1:]

    if os.environ.get(ENV_SKIP_BOOTSTRAP):
        return run_binary(resolve_target().leindex_binary, args)

    try:
        binary, freshly_installed = ensure_leindex_installed(
            interactive=sys.stdin.isatty()
        )
    except BootstrapError as error:
        print(f"LeIndex bootstrap failed: {error}", file=sys.stderr)
        return 1

    if install_only:
        print(f"LeIndex {desired_version()} is installed at {binary}")
        return 0

    # VAL-PYPI-004: after a fresh bootstrap, guide the user toward
    # ``leindex setup`` so ORT + models are configured. We only auto-launch
    # the interactive wizard for the bare ``leindex`` invocation (no args);
    # if the user passed a subcommand we let that run and only emit a hint.
    if freshly_installed and not args and setup_hint_eligible():
        interactive = sys.stdin.isatty()
        if interactive:
            return run_binary(binary, ["setup"])
        print(
            "LeIndex installed. Run `leindex setup` to enable neural search "
            "(optional; TF-IDF search works immediately).",
            file=sys.stderr,
        )
        return 0

    if freshly_installed and args and setup_hint_eligible() and not sys.stdin.isatty():
        print(
            "Tip: run `leindex setup` to enable neural search.",
            file=sys.stderr,
        )

    return run_binary(binary, args)


def setup_main(argv: Sequence[str] | None = None) -> int:
    """Console-script entry point for ``leindex-setup`` (VAL-PYPI-005).

    Ensures the Rust binary is bootstrapped (just like :func:`main`), then
    runs ``leindex setup`` with any forwarded arguments. This mirrors the
    contract: ``leindex-setup --check`` runs the bundled binary's
    ``setup --check`` and reports the same exit code.
    """
    args = list(sys.argv[1:] if argv is None else argv)

    if os.environ.get(ENV_SKIP_BOOTSTRAP):
        return run_binary(resolve_target().leindex_binary, ["setup", *args])

    try:
        binary, _ = ensure_leindex_installed(interactive=sys.stdin.isatty())
    except BootstrapError as error:
        print(f"LeIndex bootstrap failed: {error}", file=sys.stderr)
        return 1

    return run_binary(binary, ["setup", *args])


def setup_hint_eligible() -> bool:
    """Whether the post-bootstrap setup hint/launch should fire.

    Disabled via ``LEINDEX_SKIP_SETUP_AFTER_BOOTSTRAP=1`` (used by tests and
    non-interactive provisioners that do not want the wrapper to redirect the
    bare ``leindex`` invocation into the setup wizard).
    """
    return not os.environ.get(ENV_SKIP_SETUP_AFTER_BOOTSTRAP)


def desired_version() -> str:
    return os.environ.get(ENV_VERSION_OVERRIDE, __version__).strip()


def resolve_target() -> InstallTarget:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).expanduser()
    cargo_bin = cargo_home / "bin"
    cargo_binary = cargo_bin / binary_name("cargo")
    leindex_binary = cargo_bin / binary_name("leindex")
    embed_binary = cargo_bin / binary_name("leindex-embed")
    return InstallTarget(
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        cargo_binary=cargo_binary,
        leindex_binary=leindex_binary,
        embed_binary=embed_binary,
    )


def binary_name(base: str) -> str:
    return f"{base}.exe" if os.name == "nt" else base


def ensure_leindex_installed(*, interactive: bool) -> tuple[Path, bool]:
    """Ensure the Rust ``leindex`` binary is installed and up to date.

    Returns ``(binary_path, freshly_installed)`` where ``freshly_installed`` is
    ``True`` when a build/upgrade was actually performed this call. Callers use
    the flag to decide whether to offer ``leindex setup`` (VAL-PYPI-004).
    """
    target = resolve_target()
    wanted_version = desired_version()
    installed_version = read_installed_version(target.leindex_binary)

    if installed_version and version_at_least(installed_version, wanted_version):
        # VAL-PYPI-008: opportunistically ensure the worker binary exists even
        # when the main binary is current (an older partial bootstrap may have
        # installed only the main binary). We do not block the everyday path on
        # a missing worker; it is only required for neural search, and setup
        # will surface a clear message if it is absent.
        ensure_worker_present(target, interactive=interactive)
        return target.leindex_binary, False

    cargo_binary = ensure_cargo_available(target, interactive=interactive)
    install_leindex(cargo_binary, wanted_version)
    # VAL-PYPI-008: also install the worker binary so neural search works after
    # setup. This is a separate crates.io package with its own bin target.
    install_embed_worker(cargo_binary, wanted_version)

    installed_version = read_installed_version(target.leindex_binary)
    if not installed_version:
        raise BootstrapError(
            f"`cargo install {MAIN_PACKAGE}` did not create {target.leindex_binary}"
        )
    if not version_at_least(installed_version, wanted_version):
        raise BootstrapError(
            f"expected LeIndex {wanted_version}, but found {installed_version} after install"
        )

    return target.leindex_binary, True


def ensure_worker_present(target: InstallTarget, *, interactive: bool) -> None:
    """Best-effort install of a missing worker binary alongside a current main binary.

    Only triggers when the main binary is present at the wanted version but the
    worker is absent. Failures are reported on stderr and do NOT abort the run:
    the worker is only needed for neural search, which ``leindex setup`` handles
    end-to-end and reports clearly when ORT/worker are unavailable.
    """
    if target.embed_binary.is_file():
        return

    cargo = shutil.which("cargo") or (
        str(target.cargo_binary) if target.cargo_binary.is_file() else None
    )
    if not cargo:
        # No cargo available; defer to setup which will surface actionable
        # guidance. Returning keeps TF-IDF search working immediately.
        return

    try:
        install_embed_worker(Path(cargo), desired_version())
    except BootstrapError as error:
        print(
            f"Warning: could not install the leindex-embed worker: {error}",
            file=sys.stderr,
        )
        print(
            "Neural search will be unavailable until the worker is installed "
            "(`leindex setup` can retry).",
            file=sys.stderr,
        )


def ensure_cargo_available(target: InstallTarget, *, interactive: bool) -> Path:
    cargo = shutil.which("cargo")
    if cargo:
        return Path(cargo)

    if target.cargo_binary.is_file():
        return target.cargo_binary

    if not supports_rustup_install():
        raise BootstrapError(
            "Cargo is not installed, and automatic Rust installation is not supported on this platform. "
            "Please install Rust from https://rustup.rs/ and retry `pip install leindex`."
        )

    if not interactive:
        raise BootstrapError(
            "Cargo is required to install the Rust LeIndex binary. "
            "Automatic Rust installation is available, but this session is non-interactive. "
            "Install Rust from https://rustup.rs/ or rerun `leindex` in a terminal and approve the prompt."
        )

    prompt = (
        "Cargo is required to install the Rust LeIndex binary.\n"
        f"Install Rust/Cargo now so LeIndex can be installed into {target.cargo_bin}? [y/N] "
    )
    response = input(prompt).strip().lower()
    if response not in {"y", "yes"}:
        raise BootstrapError("Rust/Cargo installation was declined by the user.")

    install_cargo(target)

    cargo = shutil.which("cargo")
    if cargo:
        return Path(cargo)
    if target.cargo_binary.is_file():
        return target.cargo_binary

    raise BootstrapError("Rust installation completed, but `cargo` is still unavailable.")


def supports_rustup_install() -> bool:
    return platform.system().lower() in {"linux", "darwin", "windows"}


def install_cargo(target: InstallTarget) -> None:
    target.cargo_bin.mkdir(parents=True, exist_ok=True)

    if os.name == "nt":
        install_cargo_windows()
    else:
        install_cargo_unix()


def install_cargo_unix() -> None:
    with tempfile.NamedTemporaryFile(delete=False) as handle:
        script_path = Path(handle.name)

    try:
        download_to_file(RUSTUP_UNIX_URL, script_path)
        run_checked(["sh", str(script_path), "-y"], "Rust/Cargo installation failed")
    finally:
        script_path.unlink(missing_ok=True)


def install_cargo_windows() -> None:
    suffix = ".exe"
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as handle:
        exe_path = Path(handle.name)

    try:
        download_to_file(RUSTUP_WINDOWS_URL, exe_path)
        run_checked([str(exe_path), "-y"], "Rust/Cargo installation failed")
    finally:
        exe_path.unlink(missing_ok=True)


def download_to_file(url: str, destination: Path) -> None:
    try:
        with urllib.request.urlopen(url) as response, destination.open("wb") as handle:
            shutil.copyfileobj(response, handle)
    except OSError as error:
        raise BootstrapError(f"failed to download {url}: {error}") from error


def build_install_command(
    cargo_binary: Path, package: str, version: str, *, features: str
) -> list[str]:
    """Compose the ``cargo install`` command for a single package.

    ``--locked`` keeps the build reproducible against the published Cargo.lock,
    and ``--force`` ensures stale binaries are replaced when re-running setup.
    """
    return [
        str(cargo_binary),
        "install",
        package,
        "--locked",
        "--force",
        "--version",
        version,
        "--features",
        features,
    ]


def install_leindex(cargo_binary: Path, version: str) -> None:
    print(f"Installing LeIndex {version} via cargo...", file=sys.stderr)
    run_checked(
        build_install_command(cargo_binary, MAIN_PACKAGE, version, features=INSTALL_FEATURES),
        "LeIndex installation via cargo failed",
    )


def install_embed_worker(cargo_binary: Path, version: str) -> None:
    """Install the ``leindex-embed`` worker binary (VAL-PYPI-008)."""
    print(f"Installing leindex-embed {version} worker via cargo...", file=sys.stderr)
    run_checked(
        build_install_command(
            cargo_binary, WORKER_PACKAGE, version, features=INSTALL_FEATURES
        ),
        "leindex-embed worker installation via cargo failed",
    )


def run_checked(command: Sequence[str], message: str) -> None:
    env = os.environ.copy()
    try:
        subprocess.run(command, check=True, env=env)
    except subprocess.CalledProcessError as error:
        raise BootstrapError(f"{message}: exit code {error.returncode}") from error
    except OSError as error:
        raise BootstrapError(f"{message}: {error}") from error


def run_binary(binary: Path, args: Iterable[str]) -> int:
    """Run the installed Rust binary, relaying signals and exit codes.

    VAL-PYPI-007: when the user runs ``leindex <args>``, SIGINT (Ctrl+C) and
    SIGTERM propagate to the child so MCP servers and CI behave identically to
    the bare Rust binary. We return the child's real exit code instead of a
    Python ``KeyboardInterrupt`` traceback.

    We forward BOTH SIGINT and SIGTERM explicitly. This covers both signal
    delivery paths: terminal Ctrl+C (which sends SIGINT to the foreground
    process group) and an MCP supervisor or CI runner that targets the wrapper
    PID directly. Forwarding is idempotent, so the child receiving the signal
    via the group AND via our explicit relay is harmless.
    """
    command = [str(binary), *args]
    try:
        proc = subprocess.Popen(command)
    except FileNotFoundError as error:
        raise BootstrapError(f"installed LeIndex binary not found: {binary}") from error
    except OSError as error:
        raise BootstrapError(
            f"failed to launch installed LeIndex binary: {error}"
        ) from error

    forward_signals_to(proc)

    try:
        return proc.wait()
    except KeyboardInterrupt:
        # Defensive fallback: if Python's default SIGINT handler still fires
        # (it shouldn't, since we register a forwarding handler above), wait
        # for the child to exit so we relay its actual exit code rather than
        # printing a traceback.
        try:
            return proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            return 130  # conventional: 128 + SIGINT(2)
    finally:
        restore_default_signal_handlers()


def forward_signals_to(proc: "subprocess.Popen[bytes]") -> None:
    """Install signal handlers that forward SIGINT/SIGTERM to the child.

    Relayed signals mirror the child's own handler so the responsive binary
    (which handles signals itself) gets to clean up, then the wrapper exits
    with the child's code when ``proc.wait()`` returns. Handlers are a no-op
    when the child has already exited.

    By registering an explicit SIGINT forwarder (rather than relying on
    Python's default KeyboardInterrupt translation), a SIGINT directed at the
    wrapper PID alone reaches the child even when the child is not in the
    wrapper's process group.
    """

    def make_relay(signum: int) -> Callable[[int, object], None]:
        def relay(_signum: int, _frame: object) -> None:
            if proc.poll() is None:
                try:
                    proc.send_signal(signum)
                except Exception:
                    # Swallow: process may have just exited. ``proc.wait()``
                    # below still returns the real status.
                    pass

        return relay

    for name in _forwarded_signal_names():
        signum = getattr(signal, name, None)
        if signum is None:
            continue
        try:
            previous = signal.signal(signum, make_relay(signum))
        except (OSError, ValueError):
            # Not all signals can be registered from every thread context
            # (e.g. signals registered from a non-main thread). Skip silently;
            # the foreground-process-group delivery still covers the common
            # interactive case.
            previous = None
        _previous_handlers.append((signum, previous))


def _forwarded_signal_names() -> tuple[str, ...]:
    # VAL-PYPI-007: forward both SIGINT and SIGTERM on every platform so the
    # child's own signal handling runs and the wrapper returns the child's
    # true exit code. ``signal.SIGTERM``/``SIGINT`` exist on all platforms;
    # ``SIGBREAK`` is Windows-specific and covered when present.
    names = ("SIGINT", "SIGTERM")
    if os.name == "nt":
        names = (*names, "SIGBREAK")
    return names


_previous_handlers: list[tuple[int, object]] = []


def restore_default_signal_handlers() -> None:
    """Restore the signal handlers saved by :func:`forward_signals_to`."""
    while _previous_handlers:
        signum, previous = _previous_handlers.pop()
        try:
            signal.signal(signum, previous)  # type: ignore[arg-type]
        except (OSError, ValueError):
            pass


def read_installed_version(binary: Path) -> str | None:
    if not binary.is_file():
        return None

    try:
        completed = subprocess.run(
            [str(binary), "--version"],
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        return None

    if completed.returncode != 0:
        return None

    output = f"{completed.stdout}\n{completed.stderr}"
    match = re.search(r"(\d+\.\d+\.\d+)", output)
    return match.group(1) if match else None


def version_at_least(installed: str, wanted: str) -> bool:
    return parse_version(installed) >= parse_version(wanted)


def parse_version(value: str) -> tuple[int, int, int]:
    match = re.search(r"(\d+)\.(\d+)\.(\d+)", value)
    if not match:
        raise BootstrapError(f"could not parse semantic version from {value!r}")
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]
