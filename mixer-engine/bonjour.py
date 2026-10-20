"""
Bonjour/mDNS service announcement for ua-mixer-daemon.

Advertises _uamixer._tcp. so ConsoleLink auto-discovers the daemon.
The real UA Mixer Engine advertises with the computer hostname
on port 4710 with empty TXT properties.  It also advertises
_uamixerhelper._tcp. for the Mixer Helper (port 4720); UA Console and UA
Services look that type up before they open the helper connection.
"""

import logging
import socket
import threading

log = logging.getLogger(__name__)

# Optional dependency — daemon works without it
try:
    from zeroconf import Zeroconf, ServiceInfo
    HAS_ZEROCONF = True
except ImportError:
    HAS_ZEROCONF = False


SERVICE_TYPE = "_uamixer._tcp.local."
HELPER_SERVICE_TYPE = "_uamixerhelper._tcp.local."


class BonjourAnnouncer:
    """Advertise the mixer daemon (and its helper port) via Bonjour/mDNS."""

    def __init__(self, port: int = 4710, name: str | None = None,
                 helper_port: int | None = None):
        self.port = port
        self.helper_port = helper_port
        self.name = name or _default_name()
        self.zc: "Zeroconf | None" = None
        self.infos: list = []
        self._thread: threading.Thread | None = None

    @property
    def info(self):
        """The _uamixer service (first registered), or None."""
        return self.infos[0] if self.infos else None

    def _services(self) -> list[tuple[str, int]]:
        services = [(SERVICE_TYPE, self.port)]
        if self.helper_port:
            services.append((HELPER_SERVICE_TYPE, self.helper_port))
        return services

    def start(self) -> bool:
        """Register the Bonjour services. Returns True on success.

        Without the zeroconf package this logs a warning and returns False;
        the daemon keeps running without discovery.
        """
        if not HAS_ZEROCONF:
            log.warning("zeroconf not installed — Bonjour disabled "
                        "(pip install zeroconf, or your distro's "
                        "python3-zeroconf / python-zeroconf package)")
            return False

        try:
            self.zc = Zeroconf()
            # Without address records a browser finds the instance but
            # cannot resolve it to a host to connect to.
            addrs = _local_ipv4_addresses()
            for stype, port in self._services():
                info = ServiceInfo(
                    stype,
                    f"{self.name}.{stype}",
                    port=port,
                    parsed_addresses=addrs or None,
                    server=socket.gethostname() + ".local.",
                    properties={},
                )
                self.zc.register_service(info)
                self.infos.append(info)
                log.info("Bonjour: advertising '%s' on %s port %d",
                         self.name, stype.removesuffix(".local."), port)
            return True
        except Exception:
            log.exception("Bonjour registration failed")
            self.stop()
            return False

    def start_in_background(self):
        """start() on its own thread. Registering probes the network first (about 1.7 s per
        service), and the daemon opens its ports meanwhile instead of after."""
        self._thread = threading.Thread(target=self.start, name="bonjour", daemon=True)
        self._thread.start()

    def stop(self):
        """Unregister the Bonjour services."""
        thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=5)   # a registration still running finishes first
        if self.zc:
            for info in self.infos:
                try:
                    self.zc.unregister_service(info)
                except Exception:
                    pass
            try:
                self.zc.close()
            except Exception:
                pass
            self.zc = None
            self.infos = []
            log.info("Bonjour: unregistered")


def _local_ipv4_addresses() -> list[str]:
    """Non-loopback, non-link-local IPv4 addresses of this machine."""
    found: list[str] = []
    try:
        import ifaddr  # a dependency of zeroconf
        for adapter in ifaddr.get_adapters():
            for ip in adapter.ips:
                if ip.is_IPv4 and isinstance(ip.ip, str):
                    found.append(ip.ip)
    except Exception:
        pass
    if not found:
        # Route-based guess: the source address of a UDP "connection"
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
                sock.connect(("192.0.2.1", 9))
                found.append(sock.getsockname()[0])
        except OSError:
            pass
    return [a for a in dict.fromkeys(found)
            if not a.startswith(("127.", "169.254.", "0."))]


def _default_name() -> str:
    """Generate a default service name from hostname."""
    hostname = socket.gethostname()
    # Strip .local suffix if present
    if hostname.endswith(".local"):
        hostname = hostname[:-6]
    # Replace hyphens with spaces for display (matches Apple convention)
    return hostname.replace("-", " ")
