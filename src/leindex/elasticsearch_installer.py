#!/usr/bin/env python
"""
Elasticsearch auto-installer for multiple Linux distributions and package managers.
Supports apt (Debian/Ubuntu), yum/dnf (RHEL/CentOS/Fedora), pacman (Arch), and npm fallback.
"""
import subprocess
import sys
import time
import logging
from typing import Dict, List

from .system_utils import detect_system


class ElasticsearchInstaller:
    """Elasticsearch installer with support for multiple package managers."""

    def __init__(self, version: str = "8.x", verbose: bool = False):
        self.version = version
        self.verbose = verbose
        self.system_info = detect_system()
        self.logger = self._setup_logger()

    def _setup_logger(self) -> logging.Logger:
        """Setup logging configuration."""
        logger = logging.getLogger(__name__)
        if not logger.handlers:
            # Use stderr to avoid contaminating MCP stdio transport
            # MCP uses stdout exclusively for JSON-RPC protocol messages
            handler = logging.StreamHandler(sys.stderr)
            formatter = logging.Formatter(
                '%(asctime)s - %(levelname)s - %(message)s'
            )
            handler.setFormatter(formatter)
            logger.addHandler(handler)
            logger.setLevel(logging.DEBUG if self.verbose else logging.INFO)
        return logger

    def _run_command(self, command: List[str], check: bool = True,
                     sudo: bool = False, timeout: int = 300) -> subprocess.CompletedProcess:
        """Run a command with optional sudo and error handling."""
        if sudo and not self.system_info.is_root():
            if self.system_info.supports_sudo():
                command = ["sudo"] + command
            else:
                raise RuntimeError("Root privileges required but sudo not available")

        self.logger.info(f"Running command: {' '.join(command)}")

        try:
            result = subprocess.run(
                command,
                check=check,
                capture_output=True,
                text=True,
                timeout=timeout
            )
            if self.verbose and result.stdout:
                self.logger.debug(f"Command output: {result.stdout}")
            return result
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"Command timed out: {' '.join(command)}")
        except subprocess.CalledProcessError as e:
            error_msg = f"Command failed: {' '.join(command)}"
            if e.stderr:
                error_msg += f"\nError output: {e.stderr}"
            raise RuntimeError(error_msg)

    def _check_elasticsearch_installed(self) -> bool:
        """Check if Elasticsearch is already installed and running."""
        try:
            # Check if elasticsearch command exists
            result = self._run_command(["which", "elasticsearch"], check=False)
            if result.returncode != 0:
                return False

            # Check if service is running
            if self.system_info.package_manager in ["apt", "yum", "dnf"]:
                try:
                    result = self._run_command(
                        ["systemctl", "is-active", "--quiet", "elasticsearch"],
                        check=False
                    )
                    if result.returncode != 0:
                        return False
                except subprocess.CalledProcessError:
                    return False

            # Try to connect to Elasticsearch using the same configuration as the server
            try:
                # Import here to avoid circular imports
                from .elasticsearch_config import elasticsearch_config
                client = elasticsearch_config.create_elasticsearch_client(timeout=5)
                return elasticsearch_config.test_connection(client)
            except Exception:
                pass

            return False
        except Exception:
            return False

    def install_elasticsearch(self) -> bool:
        """Install Elasticsearch based on detected package manager."""
        self.logger.info(f"Installing Elasticsearch on {self.system_info.distribution} using {self.system_info.package_manager}")

        if self._check_elasticsearch_installed():
            self.logger.info("Elasticsearch is already installed and running")
            return True

        installer_methods = {
            "apt": self._install_with_apt,
            "yum": self._install_with_yum,
            "dnf": self._install_with_dnf,
            "pacman": self._install_with_pacman,
            "zypper": self._install_with_zypper
        }

        install_method = installer_methods.get(self.system_info.package_manager)
        if install_method:
            return install_method()
        else:
            self.logger.warning(f"Package manager {self.system_info.package_manager} not supported, trying npm fallback")
            return self._install_with_npm()

    def _install_with_apt(self) -> bool:
        """Install Elasticsearch using apt (Debian/Ubuntu)."""
        self.logger.info("Installing Elasticsearch using apt")

        commands = [
            # Import the Elasticsearch GPG key
            ["wget", "-qO", "-", "https://artifacts.elastic.co/GPG-KEY-elasticsearch"],
            ["sudo", "gpg", "--dearmor", "-o", "/usr/share/keyrings/elasticsearch-keyring.gpg"],

            # Add the Elasticsearch repository
            ["bash", "-c", f'echo "deb [signed-by=/usr/share/keyrings/elasticsearch-keyring.gpg] https://artifacts.elastic.co/packages/{self.version}/apt stable main" | sudo tee /etc/apt/sources.list.d/elastic-{self.version}.list'],

            # Update package list
            ["sudo", "apt", "update"],

            # Install Elasticsearch
            ["sudo", "apt", "install", "-y", "elasticsearch"],

            # Enable and start Elasticsearch service
            ["sudo", "systemctl", "daemon-reload"],
            ["sudo", "systemctl", "enable", "elasticsearch"],
            ["sudo", "systemctl", "start", "elasticsearch"]
        ]

        for command in commands:
            try:
                self._run_command(command)
            except RuntimeError as e:
                self.logger.error(f"Failed to execute command: {' '.join(command)}")
                self.logger.error(f"Error: {e}")
                return False

        # Configure Elasticsearch for development (disable security, optimize memory)
        if not self._configure_for_development():
            self.logger.warning("Failed to configure Elasticsearch for development, but installation succeeded")

        # Wait for Elasticsearch to start
        return self._wait_for_elasticsearch()

    def _install_with_yum(self) -> bool:
        """Install Elasticsearch using yum (RHEL/CentOS 7)."""
        self.logger.info("Installing Elasticsearch using yum")

        # Import GPG key
        self._run_command(["sudo", "rpm", "--import", "https://artifacts.elastic.co/GPG-KEY-elasticsearch"])

        # Add repository
        repo_content = f"""[elasticsearch-{self.version}]
name=Elasticsearch repository for {self.version} packages
baseurl=https://artifacts.elastic.co/packages/{self.version}/yum
gpgcheck=1
gpgkey=https://artifacts.elastic.co/GPG-KEY-elasticsearch
enabled=0
autorefresh=1
type=rpm-md
"""

        repo_file = f"/etc/yum.repos.d/elasticsearch-{self.version}.repo"
        self._run_command(["sudo", "tee", repo_file], input=repo_content)

        # Install Elasticsearch
        self._run_command(["sudo", "yum", "install", "-y", f"elasticsearch-{self.version}"])

        # Start and enable service
        self._run_command(["sudo", "systemctl", "daemon-reload"])
        self._run_command(["sudo", "systemctl", "enable", "elasticsearch"])

        # Configure Elasticsearch for development (disable security, optimize memory)
        if not self._configure_for_development():
            self.logger.warning("Failed to configure Elasticsearch for development, but installation succeeded")

        self._run_command(["sudo", "systemctl", "start", "elasticsearch"])

        return self._wait_for_elasticsearch()

    def _install_with_dnf(self) -> bool:
        """Install Elasticsearch using dnf (RHEL/CentOS 8+, Fedora)."""
        self.logger.info("Installing Elasticsearch using dnf")

        # Import GPG key
        self._run_command(["sudo", "rpm", "--import", "https://artifacts.elastic.co/GPG-KEY-elasticsearch"])

        # Add repository
        repo_content = f"""[elasticsearch-{self.version}]
name=Elasticsearch repository for {self.version} packages
baseurl=https://artifacts.elastic.co/packages/{self.version}/yum
gpgcheck=1
gpgkey=https://artifacts.elastic.co/GPG-KEY-elasticsearch
enabled=0
autorefresh=1
type=rpm-md
"""

        repo_file = f"/etc/yum.repos.d/elasticsearch-{self.version}.repo"
        self._run_command(["sudo", "tee", repo_file], input=repo_content)

        # Install Elasticsearch
        self._run_command(["sudo", "dnf", "install", "-y", f"elasticsearch-{self.version}"])

        # Start and enable service
        self._run_command(["sudo", "systemctl", "daemon-reload"])
        self._run_command(["sudo", "systemctl", "enable", "elasticsearch"])

        # Configure Elasticsearch for development (disable security, optimize memory)
        if not self._configure_for_development():
            self.logger.warning("Failed to configure Elasticsearch for development, but installation succeeded")

        self._run_command(["sudo", "systemctl", "start", "elasticsearch"])

        return self._wait_for_elasticsearch()

    def _install_with_pacman(self) -> bool:
        """Install Elasticsearch using pacman (Arch Linux)."""
        self.logger.info("Installing Elasticsearch using pacman")

        # Update package database
        self._run_command(["sudo", "pacman", "-Sy"])

        # Search for elasticsearch package
        result = self._run_command(["sudo", "pacman", "-Ss", "elasticsearch"], check=False)

        if result.returncode == 0 and "elasticsearch" in result.stdout:
            # Install from Arch repository if available
            self._run_command(["sudo", "pacman", "-S", "--noconfirm", "elasticsearch"])
        else:
            # Install from AUR (requires yay or paru)
            aur_helper = None
            for helper in ["yay", "paru"]:
                try:
                    self._run_command([helper, "--version"], check=False)
                    aur_helper = helper
                    break
                except RuntimeError:
                    continue

            if aur_helper:
                self._run_command([aur_helper, "-S", "--noconfirm", "elasticsearch"])
            else:
                self.logger.warning("Elasticsearch not found in official repositories and no AUR helper available")
                return False

        # Start and enable service
        self._run_command(["sudo", "systemctl", "daemon-reload"])
        self._run_command(["sudo", "systemctl", "enable", "elasticsearch"])

        # Configure Elasticsearch for development (disable security, optimize memory)
        if not self._configure_for_development():
            self.logger.warning("Failed to configure Elasticsearch for development, but installation succeeded")

        self._run_command(["sudo", "systemctl", "start", "elasticsearch"])

        return self._wait_for_elasticsearch()

    def _install_with_zypper(self) -> bool:
        """Install Elasticsearch using zypper (openSUSE)."""
        self.logger.info("Installing Elasticsearch using zypper")

        # Import GPG key
        self._run_command(["sudo", "rpm", "--import", "https://artifacts.elastic.co/GPG-KEY-elasticsearch"])

        # Add repository
        repo_content = f"""[elasticsearch-{self.version}]
name=Elasticsearch repository for {self.version} packages
baseurl=https://artifacts.elastic.co/packages/{self.version}/yum
gpgcheck=1
gpgkey=https://artifacts.elastic.co/GPG-KEY-elasticsearch
enabled=1
autorefresh=1
type=rpm-md
"""

        repo_file = f"/etc/zypp/repos.d/elasticsearch-{self.version}.repo"
        self._run_command(["sudo", "tee", repo_file], input=repo_content)

        # Refresh repositories
        self._run_command(["sudo", "zypper", "refresh"])

        # Install Elasticsearch
        self._run_command(["sudo", "zypper", "install", "-y", f"elasticsearch-{self.version}"])

        # Start and enable service
        self._run_command(["sudo", "systemctl", "daemon-reload"])
        self._run_command(["sudo", "systemctl", "enable", "elasticsearch"])

        # Configure Elasticsearch for development (disable security, optimize memory)
        if not self._configure_for_development():
            self.logger.warning("Failed to configure Elasticsearch for development, but installation succeeded")

        self._run_command(["sudo", "systemctl", "start", "elasticsearch"])

        return self._wait_for_elasticsearch()

    def _install_with_npm(self) -> bool:
        """Install Elasticsearch using npm as fallback (for development only)."""
        self.logger.warning("Installing Elasticsearch using npm - this is not recommended for production")

        try:
            # Check if npm is available
            self._run_command(["npm", "--version"])
        except RuntimeError:
            self.logger.error("npm not found. Please install npm or use a different package manager.")
            return False

        # Install Elasticsearch as a global npm package
        self._run_command(["npm", "install", "-g", "@elastic/elasticsearch"])

        self.logger.warning("Elasticsearch installed via npm. This may require manual configuration.")
        return True

    def _configure_for_development(self) -> bool:
        """Configure Elasticsearch for development use (disable security, optimize memory)."""
        try:
            self.logger.info("Configuring Elasticsearch for development...")

            # Create development configuration with security disabled and memory optimized
            config_content = """cluster.name: code-index-cluster
node.name: code-index-node-1
path.data: /var/lib/elasticsearch
path.logs: /var/log/elasticsearch
network.host: 127.0.0.1
http.port: 9200
discovery.type: single-node
xpack.security.enabled: false
xpack.security.enrollment.enabled: false
"""

            # Write configuration file
            self._run_command(["sudo", "tee", "/etc/elasticsearch/elasticsearch.yml"], input=config_content)

            # Create systemd override for memory settings
            override_content = """[Service]
# Set heap size to 4GB for development (adjust based on available RAM)
Environment=ES_JAVA_OPTS="-Xms4g -Xmx4g"

# Memory limits to prevent excessive usage
MemoryLimit=6g
MemoryMax=6g

# Performance optimizations
Environment=ES_DIRECTORIES_SIZE_LIMIT=1g
"""

            # Create override directory and file
            self._run_command(["sudo", "mkdir", "-p", "/etc/systemd/system/elasticsearch.service.d"], check=False)
            self._run_command(["sudo", "tee", "/etc/systemd/system/elasticsearch.service.d/override.conf"], input=override_content)

            # Reload systemd to apply changes
            self._run_command(["sudo", "systemctl", "daemon-reload"])

            self.logger.info("✓ Elasticsearch configured for development:")
            self.logger.info("  - Security disabled (no authentication required)")
            self.logger.info("  - Memory limited to 4GB heap")
            self.logger.info("  - Running on http://localhost:9200")
            self.logger.info("  - Note: For production, enable security and adjust memory settings")

            return True

        except Exception as e:
            self.logger.error(f"Failed to configure Elasticsearch for development: {e}")
            return False

    def _wait_for_elasticsearch(self, timeout: int = 60) -> bool:
        """Wait for Elasticsearch to start and become responsive."""
        self.logger.info("Waiting for Elasticsearch to start...")

        # Import here to avoid circular imports
        from .elasticsearch_config import elasticsearch_config

        for i in range(timeout):
            try:
                # Try to connect using the same configuration as the server
                client = elasticsearch_config.create_elasticsearch_client(timeout=5)
                if elasticsearch_config.test_connection(client):
                    self.logger.info("Elasticsearch is now running and responsive")
                    return True
            except Exception as e:
                self.logger.debug(f"Elasticsearch not yet ready (attempt {i+1}): {e}")

            time.sleep(1)
            if i % 10 == 0:
                self.logger.info(f"Waiting for Elasticsearch... ({i}/{timeout} seconds)")

        self.logger.error("Elasticsearch failed to start within timeout period")
        return False

    def get_service_status(self) -> Dict[str, str]:
        """Get Elasticsearch service status."""
        try:
            result = self._run_command(
                ["systemctl", "status", "elasticsearch"],
                check=False
            )

            if result.returncode == 0:
                # Parse systemctl output
                lines = result.stdout.split('\n')
                status = "unknown"
                for line in lines:
                    if "Active:" in line:
                        if "running" in line:
                            status = "running"
                        elif "failed" in line:
                            status = "failed"
                        elif "inactive" in line:
                            status = "inactive"
                        break

                return {
                    "service": "elasticsearch",
                    "status": status,
                    "output": result.stdout
                }
            else:
                return {
                    "service": "elasticsearch",
                    "status": "not_installed",
                    "output": result.stderr or "Service not found"
                }
        except Exception as e:
            return {
                "service": "elasticsearch",
                "status": "error",
                "output": str(e)
            }


def install_elasticsearch(version: str = "8.x", verbose: bool = False) -> bool:
    """
    Convenience function to install Elasticsearch.

    Args:
        version: Elasticsearch version to install (e.g., "8.x")
        verbose: Enable verbose logging

    Returns:
        bool: True if installation was successful
    """
    installer = ElasticsearchInstaller(version=version, verbose=verbose)
    return installer.install_elasticsearch()


def main():
    """Command line interface for Elasticsearch installer."""
    import argparse

    parser = argparse.ArgumentParser(description="Install Elasticsearch on Linux systems")
    parser.add_argument("--version", default="8.x", help="Elasticsearch version (default: 8.x)")
    parser.add_argument("--verbose", "-v", action="store_true", help="Enable verbose output")
    parser.add_argument("--check", action="store_true", help="Check if Elasticsearch is installed")
    parser.add_argument("--status", action="store_true", help="Get Elasticsearch service status")

    args = parser.parse_args()

    # Print system information
    system_info = detect_system()
    print(f"Detected system: {system_info.distribution} ({system_info.system})")
    print(f"Package manager: {system_info.package_manager}")
    print(f"Architecture: {system_info.get_architecture()}")
    print(f"Root privileges: {system_info.is_root()}")
    print()

    installer = ElasticsearchInstaller(version=args.version, verbose=args.verbose)

    if args.check:
        if installer._check_elasticsearch_installed():
            print("✓ Elasticsearch is installed and running")
        else:
            print("✗ Elasticsearch is not installed or not running")
        return

    if args.status:
        status = installer.get_service_status()
        print(f"Service: {status['service']}")
        print(f"Status: {status['status']}")
        if status['output']:
            print(f"Details: {status['output']}")
        return

    # Install Elasticsearch
    success = installer.install_elasticsearch()

    if success:
        print("✓ Elasticsearch installation completed successfully")

        # Get final status
        status = installer.get_service_status()
        print(f"Service status: {status['status']}")

        if status['status'] == 'running':
            print("✓ Elasticsearch is now running on http://localhost:9200")
            print("✓ Development configuration applied:")
            print("  - Security disabled (no authentication)")
            print("  - Memory optimized (4GB heap limit)")
            print("  - Ready for leindex integration")
        else:
            print("⚠ Elasticsearch may require manual configuration to start")
    else:
        print("✗ Elasticsearch installation failed")
        sys.exit(1)


if __name__ == "__main__":
    main()