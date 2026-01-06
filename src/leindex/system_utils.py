#!/usr/bin/env python
"""
System detection utilities for identifying Linux distribution and package manager.
"""
import platform
import subprocess
import os
import time
import asyncio
from typing import Dict, Optional, Tuple, List
from pathlib import Path


class SystemInfo:
    """System information and package manager detection."""

    def __init__(self):
        self.system = platform.system().lower()
        self.distribution = self._detect_distribution()
        self.package_manager = self._detect_package_manager()
        self.version = self._get_version()

    def _detect_distribution(self) -> str:
        """Detect Linux distribution."""
        if self.system != 'linux':
            return self.system

        # Try multiple methods to detect distribution
        methods = [
            self._detect_from_os_release,
            self._detect_from_lsb_release,
            self._detect_from_etc_issue,
            self._detect_from_platform_module
        ]

        for method in methods:
            distro = method()
            if distro:
                return distro.lower()

        return "unknown"

    def _detect_from_os_release(self) -> Optional[str]:
        """Detect distribution from /etc/os-release."""
        os_release = Path("/etc/os-release")
        if not os_release.exists():
            return None

        try:
            with open(os_release, 'r') as f:
                for line in f:
                    line = line.strip()
                    if line.startswith("ID="):
                        return line.split("=")[1].strip('"')
                    elif line.startswith("ID_LIKE="):
                        # For Ubuntu derivatives, etc.
                        return line.split("=")[1].strip('"').split()[0]
        except (IOError, OSError):
            pass

        return None

    def _detect_from_lsb_release(self) -> Optional[str]:
        """Detect distribution from lsb_release command."""
        try:
            result = subprocess.run(
                ["lsb_release", "-si"],
                capture_output=True,
                text=True,
                timeout=5
            )
            if result.returncode == 0:
                return result.stdout.strip().lower()
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError, FileNotFoundError):
            pass

        return None

    def _detect_from_etc_issue(self) -> Optional[str]:
        """Detect distribution from /etc/issue."""
        etc_issue = Path("/etc/issue")
        if not etc_issue.exists():
            return None

        try:
            with open(etc_issue, 'r') as f:
                content = f.read().lower()
                if "ubuntu" in content:
                    return "ubuntu"
                elif "debian" in content:
                    return "debian"
                elif "centos" in content:
                    return "centos"
                elif "red hat" in content or "rhel" in content:
                    return "rhel"
                elif "fedora" in content:
                    return "fedora"
                elif "arch" in content:
                    return "arch"
                elif "opensuse" in content:
                    return "opensuse"
        except (IOError, OSError):
            pass

        return None

    def _detect_from_platform_module(self) -> Optional[str]:
        """Detect distribution using platform module."""
        try:
            distro_info = platform.freedesktop_os_release()
            return distro_info.get("ID", "unknown").lower()
        except (AttributeError, OSError):
            # Fallback for older Python versions
            distro = platform.linux_distribution()
            if distro and distro[0]:
                return distro[0].lower()

        return None

    def _detect_package_manager(self) -> str:
        """Detect the primary package manager."""
        if self.system != 'linux':
            return "unknown"

        # Map distributions to package managers
        distro_to_pm = {
            "ubuntu": "apt",
            "debian": "apt",
            "linuxmint": "apt",
            "pop": "apt",
            "elementary": "apt",
            "centos": "yum",
            "rhel": "yum",
            "red hat": "yum",
            "fedora": "dnf",
            "arch": "pacman",
            "manjaro": "pacman",
            "opensuse": "zypper",
            "suse": "zypper"
        }

        # First try distribution-specific mapping
        for distro_name, pm in distro_to_pm.items():
            if distro_name in self.distribution:
                return pm

        # Fallback: detect by checking if package manager binaries exist
        package_managers = [
            ("apt", "apt-get"),
            ("dnf", "dnf"),
            ("yum", "yum"),
            ("pacman", "pacman"),
            ("zypper", "zypper"),
            ("snap", "snap")
        ]

        for pm_name, pm_binary in package_managers:
            if self._command_exists(pm_binary):
                return pm_name

        return "unknown"

    def _command_exists(self, command: str) -> bool:
        """Check if a command exists on the system."""
        try:
            subprocess.run(
                ["which", command],
                capture_output=True,
                timeout=3
            )
            return True
        except (subprocess.TimeoutExpired, subprocess.CalledProcessError, FileNotFoundError):
            return False

    def _get_version(self) -> str:
        """Get system version information."""
        if self.system == 'linux':
            try:
                result = subprocess.run(
                    ["lsb_release", "-rs"],
                    capture_output=True,
                    text=True,
                    timeout=5
                )
                if result.returncode == 0:
                    return result.stdout.strip()
            except (subprocess.TimeoutExpired, subprocess.CalledProcessError, FileNotFoundError):
                pass

        return platform.version().split()[0] if platform.version() else "unknown"

    def is_root(self) -> bool:
        """Check if running with root privileges."""
        return os.geteuid() == 0

    def supports_sudo(self) -> bool:
        """Check if sudo is available."""
        return self._command_exists("sudo")

    def get_architecture(self) -> str:
        """Get system architecture."""
        return platform.machine().lower()

    def get_summary(self) -> Dict[str, str]:
        """Get complete system summary."""
        return {
            "system": self.system,
            "distribution": self.distribution,
            "package_manager": self.package_manager,
            "version": self.version,
            "architecture": self.get_architecture(),
            "is_root": str(self.is_root()),
            "supports_sudo": str(self.supports_sudo())
        }


def detect_system() -> SystemInfo:
    """Convenience function to detect system information."""
    return SystemInfo()


def print_system_info():
    """Print system information for debugging."""
    info = detect_system()
    print("System Information:")
    print(f"  OS: {info.system}")
    print(f"  Distribution: {info.distribution}")
    print(f"  Package Manager: {info.package_manager}")
    print(f"  Version: {info.version}")
    print(f"  Architecture: {info.get_architecture()}")
    print(f"  Root privileges: {info.is_root()}")
    print(f"  Sudo available: {info.supports_sudo()}")


if __name__ == "__main__":
    print_system_info()


# ============================================================================
# DOCKER/PODMAN AUTO-STARTUP FOR MCP SERVER DEPENDENCIES
# ============================================================================

class ComposeManager:
    """Manages Docker/Podman compose services for the MCP server."""

    def __init__(self, compose_file: Optional[str] = None, project_dir: Optional[str] = None):
        """
        Initialize the ComposeManager.

        Args:
            compose_file: Path to docker-compose.yml file (default: docker-compose.yml in project_dir)
            project_dir: Directory containing docker-compose.yml (default: current directory or script location)
        """
        self.compose_file = compose_file
        self.project_dir = project_dir
        self.compose_cmd = self._detect_compose_command()
        self._all_compose_cmds = self._get_all_available_compose_commands()
        self._services_cache = None

    def _get_podman_compose_env(self) -> Optional[Dict[str, str]]:
        """
        Get environment variables for running podman compose.
        Returns None if podman-compose is not available (avoiding docker-compose plugin delegation).
        """
        # First try to find podman-compose in common locations
        podman_compose_paths = [
            "/usr/bin/podman-compose",
            "/usr/local/bin/podman-compose",
        ]

        for path in podman_compose_paths:
            if os.path.exists(path):
                env = os.environ.copy()
                env["PODMAN_COMPOSE_PROVIDER"] = path
                return env

        # If no podman-compose found, return None to signal that podman compose
        # should not be used (it would delegate to docker-compose plugin which requires Docker daemon)
        return None

    def _get_all_available_compose_commands(self) -> List[List[str]]:
        """Get all available compose commands that pass --version check, in priority order."""
        commands = [
            (["podman-compose"], "podman-compose"),
            (["docker", "compose"], "docker compose"),
            (["docker-compose"], "docker-compose"),
            (["podman", "compose"], "podman compose"),  # Try this last as fallback
        ]

        available = []
        for cmd, name in commands:
            try:
                result = subprocess.run(
                    cmd + ["--version"],
                    capture_output=True,
                    timeout=5,
                    text=True
                )
                if result.returncode == 0:
                    available.append(cmd)
                    print(f"[ComposeManager] Found available compose: {name}")
            except (subprocess.TimeoutExpired, subprocess.CalledProcessError, FileNotFoundError):
                continue

        return available

    def _detect_compose_command(self) -> Optional[List[str]]:
        """
        Detect available compose command.
        Priority: podman-compose > docker compose > docker-compose > podman compose
        """
        # Get all available commands and return the first one
        all_cmds = self._get_all_available_compose_commands()
        return all_cmds[0] if all_cmds else None

    def _get_compose_file_path(self) -> Optional[Path]:
        """Find the docker-compose.yml file."""
        if self.compose_file:
            return Path(self.compose_file)

        # Search in project_dir or script location
        search_paths = []
        if self.project_dir:
            search_paths.append(Path(self.project_dir))
        else:
            # Try current directory, then script location
            search_paths.append(Path.cwd())
            search_paths.append(Path(__file__).parent.parent.parent)  # Go up to project root

        for base_path in search_paths:
            compose_path = base_path / "docker-compose.yml"
            if compose_path.exists():
                return compose_path

            # Also check for podman-compose.yml alternative
            compose_path = base_path / "podman-compose.yml"
            if compose_path.exists():
                return compose_path

        return None

    def get_service_status(self) -> Dict[str, str]:
        """
        Get status of all services defined in compose file.

        Returns:
            Dict mapping service names to their status ('running', 'exited', 'not-found', etc.)
        """
        if not self.compose_cmd:
            return {}

        compose_file = self._get_compose_file_path()
        if not compose_file:
            return {}

        try:
            # Use ps -f json which works with both docker-compose and podman-compose
            # Note: podman-compose doesn't support --services flag, so we parse all containers
            cmd = self.compose_cmd + ["-f", str(compose_file), "ps", "-f", "json"]
            # For podman compose, skip if podman-compose is not available
            if self.compose_cmd[0] == "podman":
                env = self._get_podman_compose_env()
                if env is None:
                    return {}
            else:
                env = None

            result = subprocess.run(
                cmd,
                capture_output=True,
                timeout=10,
                text=True,
                cwd=compose_file.parent,
                env=env
            )

            if result.returncode == 0:
                import json
                containers = json.loads(result.stdout)
                service_status = {}
                for container in containers:
                    # Extract service name from labels (works for both docker and podman)
                    labels = container.get("Labels", {})
                    service_name = labels.get("com.docker.compose.service")
                    if service_name:
                        # Get the state (running, exited, etc.)
                        state = container.get("State", "unknown")
                        service_status[service_name] = state
                return service_status
        except (subprocess.TimeoutExpired, json.JSONDecodeError, Exception):
            pass

        return {}

    def _try_with_fallback(self, operation: str, compose_file: Path, *args) -> Tuple[bool, str]:
        """
        Try running a compose operation, falling back to alternative compose commands if the first fails.

        Args:
            operation: The compose operation (e.g., "up", "down", "ps")
            compose_file: Path to the compose file
            *args: Additional arguments for the compose command

        Returns:
            Tuple of (success, output_or_error_message)
        """
        if not self._all_compose_cmds:
            return False, "No compose command available"

        last_error = None
        for cmd in self._all_compose_cmds:
            # Skip podman compose if podman-compose is not available
            # (it would delegate to docker-compose plugin which requires Docker daemon)
            if cmd[0] == "podman":
                env = self._get_podman_compose_env()
                if env is None:
                    print(f"[ComposeManager] Skipping {' '.join(cmd)}: podman-compose not available (would delegate to docker-compose plugin)")
                    continue
            else:
                env = None

            try:
                full_cmd = cmd + ["-f", str(compose_file), operation] + list(args)
                print(f"[ComposeManager] Trying: {' '.join(full_cmd)}")

                result = subprocess.run(
                    full_cmd,
                    capture_output=True,
                    timeout=120,
                    text=True,
                    cwd=compose_file.parent,
                    env=env
                )

                if result.returncode == 0:
                    print(f"[ComposeManager] Success with: {' '.join(cmd)}")
                    # Update the primary command to this successful one for future use
                    self.compose_cmd = cmd
                    return True, result.stdout
                else:
                    last_error = result.stderr or result.stdout
                    print(f"[ComposeManager] Failed with {' '.join(cmd)}: {last_error[:200] if last_error else 'unknown error'}")
                    continue

            except subprocess.TimeoutExpired:
                last_error = f"Timeout with {' '.join(cmd)}"
                print(f"[ComposeManager] {last_error}")
                continue
            except Exception as e:
                last_error = str(e)
                print(f"[ComposeManager] Error with {' '.join(cmd)}: {e}")
                continue

        return False, last_error or "All compose commands failed"

    def start_services(self, detached: bool = True) -> bool:
        """
        Start all services defined in compose file.

        Args:
            detached: Run in detached mode (background)

        Returns:
            True if services were started successfully
        """
        if not self._all_compose_cmds:
            print("[ComposeManager] No compose command available")
            return False

        compose_file = self._get_compose_file_path()
        if not compose_file:
            print("[ComposeManager] No compose file found")
            return False

        args = ["-d"] if detached else []
        success, output = self._try_with_fallback("up", compose_file, *args)

        if success:
            print("[ComposeManager] Services started successfully")
        else:
            print(f"[ComposeManager] Failed to start services: {output}")
        return success

    def stop_services(self) -> bool:
        """Stop all services. Returns True if successful."""
        if not self._all_compose_cmds:
            return False

        compose_file = self._get_compose_file_path()
        if not compose_file:
            return False

        success, _ = self._try_with_fallback("down", compose_file)
        return success

    def is_service_healthy(self, service_name: str, timeout: int = 60) -> bool:
        """
        Check if a service is healthy by checking its healthcheck status.

        Args:
            service_name: Name of the service to check
            timeout: Maximum time to wait for health (seconds)

        Returns:
            True if service is healthy
        """
        if not self.compose_cmd:
            return False

        compose_file = self._get_compose_file_path()
        if not compose_file:
            return False

        start_time = time.time()
        while time.time() - start_time < timeout:
            try:
                # Use ps -f json and filter for the service (works with both docker and podman)
                # Note: podman-compose doesn't support passing service name as argument
                cmd = self.compose_cmd + ["-f", str(compose_file), "ps", "-f", "json"]
                # For podman compose, skip if podman-compose is not available
                if self.compose_cmd[0] == "podman":
                    env = self._get_podman_compose_env()
                    if env is None:
                        return False
                else:
                    env = None

                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    timeout=5,
                    text=True,
                    cwd=compose_file.parent,
                    env=env
                )

                if result.returncode == 0:
                    import json
                    containers = json.loads(result.stdout)
                    # Find the container for the requested service
                    for container in containers:
                        labels = container.get("Labels", {})
                        container_service = labels.get("com.docker.compose.service")
                        if container_service == service_name:
                            # Check health status
                            # For podman, check Status field which contains "(healthy)"
                            # For docker, check Health field
                            status = container.get("Status", "")
                            health = container.get("Health", "")

                            # Check if healthy (various formats)
                            is_healthy = (
                                health == "healthy" or  # Docker format
                                "(healthy)" in status or  # Podman format
                                ("healthy" in status.lower() and container.get("State") == "running")
                            )

                            # Also accept running with no healthcheck as healthy
                            is_running = container.get("State") == "running"

                            if is_healthy or (is_running and not health):
                                return True
                            # If running but not healthy yet, continue waiting
                            elif is_running:
                                break

                time.sleep(2)
            except Exception:
                time.sleep(2)
                continue

        return False

    def wait_for_services(self, services: List[str], timeout: int = 120) -> bool:
        """
        Wait for all specified services to become healthy.

        Args:
            services: List of service names to wait for
            timeout: Maximum time to wait per service (seconds)

        Returns:
            True if all services became healthy
        """
        for service in services:
            if not self.is_service_healthy(service, timeout):
                print(f"[ComposeManager] Service '{service}' did not become healthy")
                return False
            print(f"[ComposeManager] Service '{service}' is healthy")
        return True


def ensure_infrastructure_running(
    required_services: Optional[List[str]] = None,
    timeout: int = 120,
    auto_start: bool = True
) -> bool:
    """
    Ensure that the required infrastructure services are running.
    Auto-starts services if they are not running and auto_start is True.

    Args:
        required_services: List of services that must be running (default: ['db'])
        timeout: Timeout for waiting for services to become healthy (seconds)
        auto_start: Whether to auto-start services if not running

    Returns:
        True if all required services are running
    """
    if required_services is None:
        required_services = ["db"]

    compose_mgr = ComposeManager()

    # Check if compose command is available
    if not compose_mgr.compose_cmd:
        print("[ensure_infrastructure] No compose command found (podman/docker)")
        return False

    # Check if services are already running
    service_status = compose_mgr.get_service_status()

    # Check if all required services are present and running
    running_services = []
    missing_services = []

    for service in required_services:
        if service in service_status:
            # Check if actually healthy
            if compose_mgr.is_service_healthy(service, timeout=5):
                running_services.append(service)
            else:
                missing_services.append(service)
        else:
            missing_services.append(service)

    if running_services:
        print(f"[ensure_infrastructure] Already running: {running_services}")

    if not missing_services:
        print("[ensure_infrastructure] All required services are running")
        return True

    print(f"[ensure_infrastructure] Services not healthy: {missing_services}")

    if not auto_start:
        return False

    # Start services
    print("[ensure_infrastructure] Auto-starting services...")
    if not compose_mgr.start_services(detached=True):
        print("[ensure_infrastructure] Failed to start services")
        return False

    # Wait for services to become healthy
    print("[ensure_infrastructure] Waiting for services to become healthy...")
    return compose_mgr.wait_for_services(missing_services, timeout=timeout)


async def async_ensure_infrastructure(
    required_services: Optional[List[str]] = None,
    timeout: int = 120,
    auto_start: bool = True
) -> bool:
    """
    Async version of ensure_infrastructure_running.

    Args:
        required_services: List of services that must be running
        timeout: Timeout for waiting for services to become healthy (seconds)
        auto_start: Whether to auto-start services if not running

    Returns:
        True if all required services are running
    """
    # Run the blocking function in a thread pool
    loop = asyncio.get_event_loop()
    return await loop.run_in_executor(
        None,
        lambda: ensure_infrastructure_running(required_services, timeout, auto_start)
    )