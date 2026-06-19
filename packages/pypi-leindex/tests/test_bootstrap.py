from __future__ import annotations

import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from leindex import bootstrap


# Helpers ---------------------------------------------------------------------


def _make_target(temp_dir: Path) -> bootstrap.InstallTarget:
    cargo_home = temp_dir / ".cargo"
    cargo_bin = cargo_home / "bin"
    cargo_bin.mkdir(parents=True, exist_ok=True)
    return bootstrap.InstallTarget(
        cargo_home=cargo_home,
        cargo_bin=cargo_bin,
        cargo_binary=cargo_bin / bootstrap.binary_name("cargo"),
        leindex_binary=cargo_bin / bootstrap.binary_name("leindex"),
        embed_binary=cargo_bin / bootstrap.binary_name("leindex-embed"),
    )


# Tests -----------------------------------------------------------------------


class BootstrapTests(unittest.TestCase):
    def test_parse_version(self) -> None:
        self.assertEqual(bootstrap.parse_version("leindex 1.5.2"), (1, 5, 2))

    def test_version_at_least(self) -> None:
        self.assertTrue(bootstrap.version_at_least("1.5.2", "1.5.2"))
        self.assertTrue(bootstrap.version_at_least("1.5.3", "1.5.2"))
        self.assertFalse(bootstrap.version_at_least("1.5.1", "1.5.2"))

    def test_desired_version_honors_override(self) -> None:
        with mock.patch.dict(
            os.environ,
            {bootstrap.ENV_VERSION_OVERRIDE: "9.9.9"},
            clear=False,
        ):
            self.assertEqual(bootstrap.desired_version(), "9.9.9")

    def test_resolve_target_includes_embed_binary(self) -> None:
        # VAL-PYPI-008: the install target must know about the worker binary
        # path so it can be bootstrapped alongside the main binary.
        with tempfile.TemporaryDirectory() as temp_dir:
            with mock.patch.dict(os.environ, {"CARGO_HOME": temp_dir}, clear=False):
                target = bootstrap.resolve_target()
            self.assertEqual(target.embed_binary.name, bootstrap.binary_name("leindex-embed"))

    def test_ensure_leindex_installed_uses_existing_binary(self) -> None:
        # Use the version override so the "wanted" version is deterministic and
        # does not depend on the published __version__ string.
        with tempfile.TemporaryDirectory() as temp_dir:
            target = _make_target(Path(temp_dir))
            target.leindex_binary.write_text("")

            with (
                mock.patch.dict(
                    os.environ,
                    {
                        "CARGO_HOME": str(target.cargo_home),
                        bootstrap.ENV_VERSION_OVERRIDE: "1.5.2",
                    },
                    clear=False,
                ),
                mock.patch.object(
                    bootstrap, "read_installed_version", return_value="1.5.2"
                ) as read_version,
                mock.patch.object(bootstrap, "ensure_cargo_available") as ensure_cargo,
                mock.patch.object(bootstrap, "install_leindex") as install_leindex,
                mock.patch.object(bootstrap, "ensure_worker_present") as ensure_worker,
            ):
                resolved, fresh = bootstrap.ensure_leindex_installed(interactive=False)

            self.assertEqual(resolved, target.leindex_binary)
            self.assertFalse(fresh)
            read_version.assert_called_once_with(target.leindex_binary)
            ensure_cargo.assert_not_called()
            install_leindex.assert_not_called()
            # VAL-PYPI-008: the worker presence is checked even when the main
            # binary is already current.
            ensure_worker.assert_called_once()

    def test_ensure_leindex_installed_installs_when_outdated(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            target = _make_target(Path(temp_dir))
            target.leindex_binary.write_text("")
            target.cargo_binary.write_text("")

            with (
                mock.patch.dict(
                    os.environ,
                    {
                        "CARGO_HOME": str(target.cargo_home),
                        bootstrap.ENV_VERSION_OVERRIDE: "1.5.2",
                    },
                    clear=False,
                ),
                mock.patch.object(
                    bootstrap,
                    "read_installed_version",
                    side_effect=["1.5.1", "1.5.2"],
                ),
                mock.patch.object(
                    bootstrap, "ensure_cargo_available", return_value=target.cargo_binary
                ) as ensure_cargo,
                mock.patch.object(bootstrap, "install_leindex") as install_leindex,
                mock.patch.object(bootstrap, "install_embed_worker") as install_worker,
            ):
                resolved, fresh = bootstrap.ensure_leindex_installed(interactive=False)

            self.assertEqual(resolved, target.leindex_binary)
            # VAL-PYPI-004: a fresh install must be reported so callers can
            # offer `leindex setup`.
            self.assertTrue(fresh)
            ensure_cargo.assert_called_once()
            install_leindex.assert_called_once_with(target.cargo_binary, "1.5.2")
            # VAL-PYPI-008: the worker is bootstrapped in the same pass.
            install_worker.assert_called_once_with(target.cargo_binary, "1.5.2")

    def test_install_leindex_uses_onnx_feature(self) -> None:
        # VAL-PYPI-008: install commands must enable the `onnx` feature so the
        # binary exposes the neural-search code paths (load-dynamic keeps the
        # build ORT-free).
        captured: dict[str, list[str]] = {}

        def fake_run_checked(command, message):
            captured["cmd"] = list(command)

        with mock.patch.object(bootstrap, "run_checked", side_effect=fake_run_checked):
            bootstrap.install_leindex(Path("/fake/cargo"), "1.8.1")

        cmd = captured["cmd"]
        self.assertIn("--features", cmd)
        self.assertEqual(cmd[cmd.index("--features") + 1], "onnx")
        self.assertIn("--version", cmd)
        self.assertEqual(cmd[cmd.index("--version") + 1], "1.8.1")
        self.assertIn("leindex", cmd)

    def test_install_embed_worker_uses_onnx_feature(self) -> None:
        captured: dict[str, list[str]] = {}

        def fake_run_checked(command, message):
            captured["cmd"] = list(command)

        with mock.patch.object(bootstrap, "run_checked", side_effect=fake_run_checked):
            bootstrap.install_embed_worker(Path("/fake/cargo"), "1.8.1")

        cmd = captured["cmd"]
        self.assertIn("--features", cmd)
        self.assertEqual(cmd[cmd.index("--features") + 1], "onnx")
        # The worker package must be referenced explicitly.
        self.assertIn(bootstrap.WORKER_PACKAGE, cmd)

    def test_ensure_cargo_available_reports_noninteractive_guidance(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cargo_home = Path(temp_dir) / ".cargo"
            target = bootstrap.InstallTarget(
                cargo_home=cargo_home,
                cargo_bin=cargo_home / "bin",
                cargo_binary=cargo_home / "bin" / bootstrap.binary_name("cargo"),
                leindex_binary=cargo_home / "bin" / bootstrap.binary_name("leindex"),
                embed_binary=cargo_home / "bin" / bootstrap.binary_name("leindex-embed"),
            )

            with (
                mock.patch.object(bootstrap, "supports_rustup_install", return_value=True),
                mock.patch("shutil.which", return_value=None),
            ):
                with self.assertRaises(bootstrap.BootstrapError) as ctx:
                    bootstrap.ensure_cargo_available(target, interactive=False)

            self.assertIn("non-interactive", str(ctx.exception))

    def test_ensure_cargo_available_respects_declined_prompt(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            cargo_home = Path(temp_dir) / ".cargo"
            target = bootstrap.InstallTarget(
                cargo_home=cargo_home,
                cargo_bin=cargo_home / "bin",
                cargo_binary=cargo_home / "bin" / bootstrap.binary_name("cargo"),
                leindex_binary=cargo_home / "bin" / bootstrap.binary_name("leindex"),
                embed_binary=cargo_home / "bin" / bootstrap.binary_name("leindex-embed"),
            )

            with (
                mock.patch.object(bootstrap, "supports_rustup_install", return_value=True),
                mock.patch("shutil.which", return_value=None),
                mock.patch("builtins.input", return_value="n"),
            ):
                with self.assertRaises(bootstrap.BootstrapError) as ctx:
                    bootstrap.ensure_cargo_available(target, interactive=True)

            self.assertIn("declined", str(ctx.exception))

    def test_ensure_cargo_available_missing_cargo_nonzero_no_rustup(self) -> None:
        # VAL-PYPI-011: when cargo is missing and rustup auto-install is not
        # supported, the wrapper raises an actionable error pointing at
        # https://rustup.rs (caller returns non-zero).
        with tempfile.TemporaryDirectory() as temp_dir:
            target = _make_target(Path(temp_dir))
            with (
                mock.patch.object(bootstrap, "supports_rustup_install", return_value=False),
                mock.patch("shutil.which", return_value=None),
            ):
                with self.assertRaises(bootstrap.BootstrapError) as ctx:
                    bootstrap.ensure_cargo_available(target, interactive=True)

            message = str(ctx.exception)
            self.assertIn("rustup.rs", message)


class MainFlowTests(unittest.TestCase):
    """End-to-end behavior of main()/setup_main() with the cargo layer mocked."""

    def test_main_propagates_child_exit_code(self) -> None:
        # VAL-PYPI-007: the wrapper must relay the child's real exit code,
        # not 0.
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary", return_value=42) as run, \
            mock.patch.dict(
                os.environ,
                {bootstrap.ENV_SKIP_SETUP_AFTER_BOOTSTRAP: "1"},
                clear=False,
            ):
            ensure.return_value = (Path("/fake/leindex"), False)
            code = bootstrap.main(["search", "x"])

        self.assertEqual(code, 42)
        run.assert_called_once_with(Path("/fake/leindex"), ["search", "x"])

    def test_main_returns_nonzero_when_bootstrap_fails(self) -> None:
        with mock.patch.object(
            bootstrap,
            "ensure_leindex_installed",
            side_effect=bootstrap.BootstrapError("nope"),
        ):
            code = bootstrap.main([])

        self.assertEqual(code, 1)

    def test_main_runs_setup_after_fresh_bootstrap_interactively(self) -> None:
        # VAL-PYPI-004: under a TTY, bare `leindex` after a fresh bootstrap
        # reaches the interactive setup prompt (we delegate by running the
        # binary with ["setup"]).
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary", return_value=0) as run, \
            mock.patch("sys.stdin", isatty=mock.Mock(return_value=True)):
            ensure.return_value = (Path("/fake/leindex"), True)
            code = bootstrap.main([])

        self.assertEqual(code, 0)
        run.assert_called_once_with(Path("/fake/leindex"), ["setup"])

    def test_main_prints_setup_guidance_if_non_tty_fresh(self) -> None:
        # VAL-PYPI-004: under non-TTY (piped), the wrapper prints guidance
        # and exits without blocking.
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary") as run, \
            mock.patch("sys.stdin", isatty=mock.Mock(return_value=False)), \
            mock.patch("sys.stderr") as stderr:
            ensure.return_value = (Path("/fake/leindex"), True)
            code = bootstrap.main([])

        self.assertEqual(code, 0)
        run.assert_not_called()
        written = "".join(call.args[0] for call in stderr.write.call_args_list)
        self.assertIn("leindex setup", written)

    def test_main_skips_setup_redirect_when_args_present(self) -> None:
        # If the user passed a subcommand, we run their command (no setup
        # redirect), even after a fresh interactive bootstrap.
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary", return_value=0) as run, \
            mock.patch("sys.stdin", isatty=mock.Mock(return_value=True)):
            ensure.return_value = (Path("/fake/leindex"), True)
            code = bootstrap.main(["index", "/tmp/proj"])

        self.assertEqual(code, 0)
        run.assert_called_once_with(Path("/fake/leindex"), ["index", "/tmp/proj"])

    def test_setup_main_runs_setup_command(self) -> None:
        # VAL-PYPI-005: leindex-setup bootstraps then runs `leindex setup`.
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary", return_value=0) as run:
            ensure.return_value = (Path("/fake/leindex"), False)
            code = bootstrap.setup_main(["--check"])

        self.assertEqual(code, 0)
        run.assert_called_once_with(Path("/fake/leindex"), ["setup", "--check"])

    def test_setup_main_propagates_child_exit_code(self) -> None:
        with mock.patch.object(bootstrap, "ensure_leindex_installed") as ensure, \
            mock.patch.object(bootstrap, "run_binary", return_value=7) as run:
            ensure.return_value = (Path("/fake/leindex"), False)
            code = bootstrap.setup_main(["--neural", "--cpu"])

        self.assertEqual(code, 7)
        run.assert_called_once_with(Path("/fake/leindex"), ["setup", "--neural", "--cpu"])

    def test_setup_main_returns_nonzero_on_bootstrap_failure(self) -> None:
        with mock.patch.object(
            bootstrap,
            "ensure_leindex_installed",
            side_effect=bootstrap.BootstrapError("boom"),
        ):
            code = bootstrap.setup_main([])

        self.assertEqual(code, 1)


class SignalRelayTests(unittest.TestCase):
    """VAL-PYPI-007: signals and exit codes propagate through the wrapper."""

    def test_run_binary_returns_child_exit_code(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            script = Path(temp_dir) / "child.py"
            script.write_text("import sys; sys.exit(3)\n")
            code = bootstrap.run_binary(Path(sys.executable), [str(script)])
        self.assertEqual(code, 3)

    def test_run_binary_returns_zero_on_success(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            script = Path(temp_dir) / "ok.py"
            script.write_text("print('ok')\n")
            code = bootstrap.run_binary(Path(sys.executable), [str(script)])
        self.assertEqual(code, 0)

    def test_forward_signals_installs_and_restores_handlers(self) -> None:
        # The relay must register forwarding handlers for both SIGINT and
        # SIGTERM (VAL-PYPI-007 covers both paths) and restore the previous
        # handlers afterwards. We verify registration + restoration using a
        # SIGTERM dummy (avoiding interaction with pytest's own SIGINT
        # handling during the test run).
        dummy = mock.Mock(return_value=None)
        try:
            signal.signal(signal.SIGTERM, dummy)
        except (OSError, ValueError):
            self.skipTest("cannot register SIGTERM in this context")

        class FakeProc:
            def __init__(self) -> None:
                self.poll_count = 0
                self.sent: list[int] = []

            def poll(self) -> int | None:
                self.poll_count += 1
                return None  # always "alive" so send_signal fires

            def send_signal(self, signum: int) -> None:
                self.sent.append(signum)

        fake = FakeProc()
        bootstrap.forward_signals_to(fake)  # type: ignore[arg-type]
        # Trigger the SIGTERM handler directly.
        handler = signal.getsignal(signal.SIGTERM)
        assert callable(handler)
        handler(signal.SIGTERM, None)  # type: ignore[arg-type]
        self.assertIn(signal.SIGTERM, fake.sent)

        bootstrap.restore_default_signal_handlers()
        # The original dummy handler must be restored.
        self.assertIs(signal.getsignal(signal.SIGTERM), dummy)

        # Restore Python's default to avoid interfering with other tests.
        signal.signal(signal.SIGTERM, signal.SIG_DFL)

    def test_forwarded_signal_names_include_sigint_and_sigterm(self) -> None:
        # VAL-PYPI-007: both SIGINT and SIGTERM must be forwarded so a signal
        # sent to the wrapper PID alone reaches the child (covering both
        # terminal Ctrl+C and supervisor-directed signals).
        names = set(bootstrap._forwarded_signal_names())
        self.assertIn("SIGINT", names)
        self.assertIn("SIGTERM", names)


if __name__ == "__main__":
    unittest.main()
