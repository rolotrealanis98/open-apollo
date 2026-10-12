"""Host interface metadata for the Console 4720 NETWORK picker (captures s17/s30)."""
import json
import socket
import struct
import fcntl


def adapters():
    result = []
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        for _, name in socket.if_nameindex():
            address = ""
            try:
                request = struct.pack("256s", name.encode()[:15])
                address = socket.inet_ntoa(fcntl.ioctl(sock.fileno(), 0x8915, request)[20:24])
            except OSError:
                pass
            result.append({"adapter_name": name, "adapter_address": address})
    return result


class Network:
    def __init__(self, state, helper, publish, provider=adapters):
        self.state, self.helper, self.publish, self.provider = state, helper, publish, provider
        self.refresh()

    def helper_publish(self, key, value):
        if self.helper is None:
            return
        node = self.helper._resolve("/dante")
        if node is None:
            node = {"path": "dante", "properties": {}, "children": []}
            self.helper.root.setdefault("children", []).append(node)
        node.setdefault("properties", {})[key] = {"value": value, "read_only": False}
        self.helper._notify("/dante/" + key + "/value", value)

    def refresh(self):
        found = self.provider()
        names = {item["adapter_name"] for item in found}
        current = self.state.get_value("/dante/primary_network_adapter")
        if current not in names:
            current = next((item["adapter_name"] for item in found if item["adapter_name"] != "lo" and item["adapter_address"]), "lo" if "lo" in names else "")
        encoded = json.dumps(found, separators=(",", ":"))
        for key, value in [("available_network_adapters", encoded), ("primary_network_adapter", current)]:
            self.publish("/dante/" + key, value, "string")
            self.helper_publish(key, value)

    def set_primary(self, value):
        if not isinstance(value, str) or value not in {item["adapter_name"] for item in self.provider()}:
            raise ValueError("unknown network adapter")
        # Stores the selected interface; listening address remains explicit --host.
        self.state.set("/dante/primary_network_adapter/value", value)
        self.helper_publish("primary_network_adapter", value)
