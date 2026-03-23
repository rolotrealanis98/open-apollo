#!/usr/bin/env python3
"""Open Apollo system tray indicator.

Shows Apollo hardware status with quick actions for init, daemon
management, and PipeWire profile reload.

Dependencies: gir1.2-appindicator3-0.1 (Ubuntu) or libappindicator-gtk3 (Fedora)
"""

import os
import sys
import struct
import fcntl
import subprocess
import signal

import gi
gi.require_version('Gtk', '3.0')
gi.require_version('AppIndicator3', '0.1')
from gi.repository import Gtk, GLib, AppIndicator3

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(SCRIPT_DIR)
ICON_DIR = os.path.join(SCRIPT_DIR, 'icons')
DEVICE_PATH = '/dev/ua_apollo0'
DAEMON_LOG = '/tmp/ua-mixer-daemon.log'
POLL_INTERVAL = 5  # seconds

# Ioctl numbers for BAR0 register read
# _IOWR('U', 0x10, 8) = (3 << 30) | (ord('U') << 8) | 0x10 | (8 << 16)
IOCTL_REG_READ = (3 << 30) | (ord('U') << 8) | 0x10 | (8 << 16)


class ApolloState:
    """Snapshot of Apollo hardware + daemon state."""
    def __init__(self):
        self.driver_loaded = False
        self.device_exists = False
        self.pcie_alive = False
        self.dsp_alive = False
        self.seq_wr = 0
        self.seq_rd = 0
        self.daemon_running = False
        self.daemon_pid = None
        self.device_name = 'Apollo'

    @property
    def icon_name(self):
        if not self.driver_loaded or not self.device_exists:
            return 'apollo-gray'
        if not self.pcie_alive:
            return 'apollo-red'
        if not self.dsp_alive or not self.daemon_running:
            return 'apollo-yellow'
        return 'apollo-green'


def read_register(fd, offset):
    """Read a BAR0 register via ioctl."""
    buf = struct.pack('II', offset, 0)
    result = fcntl.ioctl(fd, IOCTL_REG_READ, buf)
    return struct.unpack('II', result)[1]


def poll_state():
    """Check Apollo hardware and daemon state."""
    state = ApolloState()

    # Driver loaded?
    try:
        with open('/proc/modules') as f:
            state.driver_loaded = 'ua_apollo' in f.read()
    except OSError:
        pass

    # Device node exists?
    state.device_exists = os.path.exists(DEVICE_PATH)

    # BAR0 register read
    if state.device_exists:
        try:
            fd = os.open(DEVICE_PATH, os.O_RDWR)
            try:
                reg0 = read_register(fd, 0x0000)
                state.pcie_alive = reg0 != 0xFFFFFFFF

                if state.pcie_alive:
                    dev_type = read_register(fd, 0x000C)
                    state.seq_wr = read_register(fd, 0x3808)
                    state.seq_rd = read_register(fd, 0x380C)
                    state.dsp_alive = (
                        state.seq_wr == state.seq_rd and state.seq_wr > 0
                    )
                    # Device name from type
                    names = {
                        0x1F: 'Apollo x4', 0x1E: 'Apollo x6',
                        0x14: 'Apollo x8', 0x15: 'Apollo x8p',
                        0x10: 'Apollo x16', 0x20: 'Apollo Twin X',
                        0x21: 'Apollo Solo',
                    }
                    state.device_name = names.get(dev_type, f'Apollo (0x{dev_type:02X})')
            finally:
                os.close(fd)
        except OSError:
            pass

    # Daemon running?
    try:
        result = subprocess.run(
            ['pgrep', '-f', 'ua_mixer_daemon'], capture_output=True
        )
        if result.returncode == 0:
            state.daemon_running = True
            state.daemon_pid = result.stdout.decode().strip().split('\n')[0]
    except OSError:
        pass

    return state


class ApolloTray:
    """System tray indicator for Open Apollo."""

    def __init__(self):
        self.state = ApolloState()
        self.indicator = AppIndicator3.Indicator.new(
            'open-apollo',
            os.path.join(ICON_DIR, 'apollo-gray'),
            AppIndicator3.IndicatorCategory.HARDWARE
        )
        self.indicator.set_status(AppIndicator3.IndicatorStatus.ACTIVE)
        self.indicator.set_icon_theme_path(ICON_DIR)
        self.build_menu()
        self.update()
        GLib.timeout_add_seconds(POLL_INTERVAL, self.update)

    def build_menu(self):
        """Build the GTK menu."""
        self.menu = Gtk.Menu()

        # Status labels
        self.lbl_device = Gtk.MenuItem(label='Apollo: checking...')
        self.lbl_device.set_sensitive(False)
        self.menu.append(self.lbl_device)

        self.lbl_dsp = Gtk.MenuItem(label='DSP: checking...')
        self.lbl_dsp.set_sensitive(False)
        self.menu.append(self.lbl_dsp)

        self.lbl_daemon = Gtk.MenuItem(label='Daemon: checking...')
        self.lbl_daemon.set_sensitive(False)
        self.menu.append(self.lbl_daemon)

        self.menu.append(Gtk.SeparatorMenuItem())

        # Actions
        item_init = Gtk.MenuItem(label='Initialize Apollo...')
        item_init.connect('activate', self.on_init)
        self.menu.append(item_init)

        item_restart = Gtk.MenuItem(label='Restart Daemon')
        item_restart.connect('activate', self.on_restart_daemon)
        self.menu.append(item_restart)

        item_stop = Gtk.MenuItem(label='Stop Daemon')
        item_stop.connect('activate', self.on_stop_daemon)
        self.menu.append(item_stop)

        item_pipewire = Gtk.MenuItem(label='Reload PipeWire Profile')
        item_pipewire.connect('activate', self.on_reload_pipewire)
        self.menu.append(item_pipewire)

        self.menu.append(Gtk.SeparatorMenuItem())

        item_log = Gtk.MenuItem(label='Open Daemon Log')
        item_log.connect('activate', self.on_open_log)
        self.menu.append(item_log)

        item_quit = Gtk.MenuItem(label='Quit')
        item_quit.connect('activate', self.on_quit)
        self.menu.append(item_quit)

        self.menu.show_all()
        self.indicator.set_menu(self.menu)

    def update(self):
        """Poll state and update UI."""
        self.state = poll_state()
        s = self.state

        # Update icon
        self.indicator.set_icon(s.icon_name)

        # Update labels
        if not s.driver_loaded:
            self.lbl_device.set_label('Driver: not loaded')
        elif not s.device_exists:
            self.lbl_device.set_label(f'{s.device_name}: disconnected')
        elif not s.pcie_alive:
            self.lbl_device.set_label(f'{s.device_name}: PCIe dead — power cycle')
        else:
            self.lbl_device.set_label(f'{s.device_name}: connected')

        if not s.device_exists:
            self.lbl_dsp.set_label('DSP: —')
        elif not s.pcie_alive:
            self.lbl_dsp.set_label('DSP: PCIe link down')
        elif s.dsp_alive:
            self.lbl_dsp.set_label(f'DSP: alive (SEQ: {s.seq_wr})')
        elif s.seq_wr == 0:
            self.lbl_dsp.set_label('DSP: not initialized')
        else:
            self.lbl_dsp.set_label(f'DSP: stalled (WR={s.seq_wr} RD={s.seq_rd})')

        if s.daemon_running:
            self.lbl_daemon.set_label(f'Daemon: running (PID {s.daemon_pid})')
        else:
            self.lbl_daemon.set_label('Daemon: stopped')

        return True  # Keep timer alive

    def run_sudo(self, cmd):
        """Run a command with pkexec for graphical sudo."""
        try:
            subprocess.Popen(['pkexec'] + cmd)
        except OSError as e:
            self.show_error(f'Failed to run command: {e}')

    def show_error(self, msg):
        """Show a simple error dialog."""
        dialog = Gtk.MessageDialog(
            message_type=Gtk.MessageType.ERROR,
            buttons=Gtk.ButtonsType.OK,
            text=msg
        )
        dialog.run()
        dialog.destroy()

    # --- Action handlers ---

    def on_init(self, _):
        init_script = os.path.join(REPO_ROOT, 'tools', 'apollo-init.sh')
        if os.path.exists(init_script):
            self.run_sudo(['bash', init_script])
        else:
            self.show_error(f'Init script not found: {init_script}')

    def on_restart_daemon(self, _):
        self.run_sudo([
            'bash', '-c',
            'pkill -f ua_mixer_daemon 2>/dev/null; sleep 1; '
            f'cd {REPO_ROOT}/mixer-engine && '
            f'nohup python3 ua_mixer_daemon.py > {DAEMON_LOG} 2>&1 &'
        ])

    def on_stop_daemon(self, _):
        self.run_sudo(['pkill', '-f', 'ua_mixer_daemon'])

    def on_reload_pipewire(self, _):
        """Re-run apollo-setup-io to recreate virtual devices."""
        subprocess.Popen(['apollo-setup-io'])

    def on_open_log(self, _):
        if os.path.exists(DAEMON_LOG):
            subprocess.Popen(['xdg-open', DAEMON_LOG])
        else:
            self.show_error(f'Log not found: {DAEMON_LOG}')

    def on_quit(self, _):
        Gtk.main_quit()


def main():
    signal.signal(signal.SIGINT, signal.SIG_DFL)  # Allow Ctrl+C
    ApolloTray()
    Gtk.main()


if __name__ == '__main__':
    main()
