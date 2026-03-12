from __future__ import annotations

import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence

from . import __version__

REPO_URL = "https://github.com/scooter-lacroix/LeIndex"
RUSTUP_UNIX_URL = "https://sh.rustup.rs"
RUSTUP_WINDOWS_URL = "https://win.rustup.rs/x86_64"
ENV_VERSION_OVERRIDE = "LEINDEX_RUST_VERSION"
ENV_SKIP_BOOTSTRAP = "LEINDEX_SKIP_PYPI_BOOTSTRAP"
INSTALL_ONLY_FLAG = "--bootstrap-install-only"


class BootstrapError(RuntimeError):
    """Raised when the PyPI bootstrapper cannot provision LeIndex."""


@dataclass(frozen=True)
class InstallTarget:
    cargo_home: Path
    cargo_bin: Path
    cargo_binary: Path
    leindex_binary: Path


def main(argv: Sequence[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    install_only = False

    if args and args[0] == INSTALL_ONLY_FLAG:
        install_only = True
        args = args[1:]

    if os.environ.get(ENV_SKIP_BOOTSTRAP):
        return run_binary(resolve_target().leindex_binary, args)

    try:
        binary = ensure_leindex_installed(interactive=sys.stdin.isatty())
    except BootstrapError as error:
        print(f"LeIndex bootstrap failed: {error}", file=sys.stderr)
        return 1

    if install_only:
        print(f"LeIndex {desired_version()} is installed at {binary}")
        return 0

    return run_binary(binary, args)


def desired_version() -> str:
    return os.environ.get(ENV_VERSION_OVERRIDE, __version__).strip()


def resolve_target() -> InstallTarget:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).expanduser()
    cargo_bin = cargo_home / "bin"
    cargo_binary = cargo_bin / binary_name("cargo")
    leindex_binary = cargo_bin / binary_name("leindex")
    return InstallTarget(
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        cargo_binary=cargo_binary,
        leindex_binary=leindex_binary,
    )


def binary_name(base: str) -> str:
    return f"{base}.exe" if os.name == "nt" else base


def ensure_leindex_installed(*, interactive: bool) -> Path:
    target = resolve_target()
    wanted_version = desired_version()
    installed_version = read_installed_version(target.leindex_binary)

    if installed_version and version_at_least(installed_version, wanted_version):
        return target.leindex_binary

    cargo_binary = ensure_cargo_available(target, interactive=interactive)
    install_leindex(cargo_binary, wanted_version)

    installed_version = read_installed_version(target.leindex_binary)
    if not installed_version:
        raise BootstrapError(
            f"`cargo install leindex` did not create {target.leindex_binary}"
        )
    if not version_at_least(installed_version, wanted_version):
        raise BootstrapError(
            f"expected LeIndex {wanted_version}, but found {installed_version} after install"
        )

    return target.leindex_binary


def ensure_cargo_available(target: InstallTarget, *, interactive: bool) -> Path:
    cargo = shutil.which("cargo")
    if cargo:
        return Path(cargo)

    if target.cargo_binary.is_file():
        return target.cargo_binary

    if not supports_rustup_install():
        raise BootstrapError(
            "Cargo is not installed, and automatic Rust installation is not supported on this platform. "
            f"Please install Rust from https://rustup.rs/ and retry `pip install leindex`."
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


def install_leindex(cargo_binary: Path, version: str) -> None:
    print(f"Installing LeIndex {version} via cargo...", file=sys.stderr)
    run_checked(
        [
            str(cargo_binary),
            "install",
            "leindex",
            "--locked",
            "--force",
            "--version",
            version,
        ],
        "LeIndex installation via cargo failed",
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
    command = [str(binary), *args]
    try:
        completed = subprocess.run(command, check=False)
    except FileNotFoundError as error:
        raise BootstrapError(f"installed LeIndex binary not found: {binary}") from error
    except OSError as error:
        raise BootstrapError(f"failed to launch installed LeIndex binary: {error}") from error
    return completed.returncode


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
    return tuple(int(part) for part in match.groups())
