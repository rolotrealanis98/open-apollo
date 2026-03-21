"""
Bonjour/mDNS service announcement for ua-mixer-daemon.

Advertises _uamixer._tcp. so ConsoleLink auto-discovers the daemon.
The real UA Mixer Engine advertises with the computer hostname
on port 4710 with empty TXT properties.
"""

import logging
import socket

log = logging.getLogger(__name__)

# Optional dependency — daemon works without it
try:
    from zeroconf import Zeroconf, ServiceInfo
    HAS_ZEROCONF = True
except ImportError:
    HAS_ZEROCONF = False


SERVICE_TYPE = "_uamixer._tcp.local."


class BonjourAnnouncer:
    """Advertise the mixer daemon via Bonjour/mDNS."""

    def __init__(self, port: int = 4710, name: str | None = None):
        self.port = port
        self.name = name or _default_name()
        self.zc: "Zeroconf | None" = None
        self.info: "ServiceInfo | None" = None

    def start(self) -> bool:
        """Register the Bonjour service. Returns True on success."""
        if not HAS_ZEROCONF:
            log.warning("zeroconf not installed — Bonjour disabled "
                        "(pip install zeroconf)")
            return False

        try:
            self.zc = Zeroconf()
            self.info = ServiceInfo(
                SERVICE_TYPE,
                f"{self.name}.{SERVICE_TYPE}",
                port=self.port,
                server=socket.gethostname() + ".local.",
                properties={},
            )
            self.zc.register_service(self.info)
            log.info("Bonjour: advertising '%s' on _uamixer._tcp port %d",
                     self.name, self.port)
            return True
        except Exception:
            log.exception("Bonjour registration failed")
            return False

    def stop(self):
        """Unregister the Bonjour service."""
        if self.zc and self.info:
            try:
                self.zc.unregister_service(self.info)
            except Exception:
                pass
            try:
                self.zc.close()
            except Exception:
                pass
            self.zc = None
            self.info = None
            log.info("Bonjour: unregistered")


def _default_name() -> str:
    """Generate a default service name from hostname."""
    hostname = socket.gethostname()
    # Strip .local suffix if present
    if hostname.endswith(".local"):
        hostname = hostname[:-6]
    # Replace hyphens with spaces for display (matches Apple convention)
    return hostname.replace("-", " ")
