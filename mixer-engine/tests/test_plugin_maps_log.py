"""setup-plugin-hosting.sh writes the mapper's log only through scripts/oa_redact.py, and streams it
to setup (and the wizard) without the redactor's time stamps."""
import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SETUP = (ROOT / 'scripts/setup-plugin-hosting.sh').read_text()


def test_build_log_is_written_only_through_the_redactor():
    assert re.search(r"""MAP_CMD=.*\| python3 -u '\$REDACT' --append '\$LOG'""", SETUP)
    assert 'ExecStart=/bin/sh -c "$MAP_CMD"' in SETUP
    assert 'StandardOutput=append' not in SETUP and '>> "$LOG"' not in SETUP
    assert 'OA_INSTALL_LOG' not in SETUP
    assert not re.search(r'OA_INSTALL_LOG', (ROOT / 'tools/plugin-mapper/plugin_mapper/__main__.py').read_text())


def test_redacted_lines_keep_the_wizard_markers(tmp_path):
    log = tmp_path / 'build.log'
    home = os.path.expanduser('~')
    lines = f'Mapping 1/2: UAD X (mono, stereo)… {home}/x\n::progress plugins 1 2 40 UAD X\n'
    subprocess.run(['python3', str(ROOT / 'scripts/oa_redact.py'), '--append', str(log)],
                   input=lines, text=True, check=True)
    assert oct(log.stat().st_mode & 0o777) == '0o600'
    raw = log.read_text()
    assert home not in raw and re.match(r'\d\d:\d\d:\d\d ', raw)
    sed = re.search(r"sed -E '([^']+)'", SETUP).group(1)
    shown = subprocess.run(['sed', '-E', sed], input=raw, text=True, capture_output=True, check=True).stdout
    assert shown.splitlines()[1] == '::progress plugins 1 2 40 UAD X'
