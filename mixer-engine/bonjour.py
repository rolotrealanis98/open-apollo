"""mDNS announcement for ua-mixer-daemon via the `avahi-publish` subprocess.

Advertises _uamixer._tcp so ConsoleLink auto-discovers the daemon, matching the
real UA Mixer Engine (hostname service name, port 4710, empty TXT properties).
"""

import logging
import shutil
import socket
import subprocess
import time

log = logging.getLogger(__name__)

SERVICE_TYPE = "_uamixer._tcp"


class BonjourAnnouncer:
    """Advertise the mixer daemon via mDNS using `avahi-publish`."""

    def __init__(self, port: int = 4710, name: str | None = None):
        self.port = port
        self.name = name or _default_name()
        self.proc: "subprocess.Popen | None" = None

    def start(self) -> bool:
        """Publish the service. True only if the child is still alive."""
        if not self.name:
            log.warning("Bonjour disabled — empty service name")
            return False
        if not shutil.which("avahi-publish"):
            log.warning("avahi-publish not found — Bonjour disabled "
                        "(install avahi-utils)")
            return False
        # List form (no shell), so a user-supplied --name cannot inject a shell
        # command. name is a trusted local CLI arg (default: hostname, which can't
        # start with '-'); a stray leading-dash name would be misparsed by
        # avahi-publish's getopt and exit immediately — caught by the poll() check
        # below and reported as a graceful disable, never executed as an option.
        proc = subprocess.Popen(
            ["avahi-publish", "-s", self.name, SERVICE_TYPE, str(self.port)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.3)
        if proc.poll() is not None:
            log.warning("Bonjour disabled — avahi-publish exited immediately "
                        "(is avahi-daemon running?)")
            return False
        self.proc = proc
        log.info("Bonjour: advertising '%s' on %s port %d",
                 self.name, SERVICE_TYPE, self.port)
        return True

    def stop(self):
        """Terminate the avahi-publish child."""
        if not self.proc:
            return
        self.proc.terminate()
        try:
            self.proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.proc = None
        log.info("Bonjour: unregistered")


def _default_name() -> str:
    """Generate a default service name from hostname."""
    hostname = socket.gethostname()
    # Strip .local suffix if present
    if hostname.endswith(".local"):
        hostname = hostname[:-6]
    # Replace hyphens with spaces for display (matches Apple convention)
    return hostname.replace("-", " ")
