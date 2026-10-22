#!/usr/bin/env python3
import importlib.util
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / 'player-client/scripts/screentra-display-governance.py'
spec = importlib.util.spec_from_file_location('screentra_display_governance_files', MODULE_PATH)
governance = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(governance)


class DisplayGovernanceFileTests(unittest.TestCase):
    def test_load_config_rejects_symlinked_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside.json'
            outside.write_text('{"max_displays":99}', encoding='utf-8')
            redirected = root / 'config.json'
            redirected.symlink_to(outside)
            self.assertEqual(governance.load_config(redirected), governance.CONFIG_DEFAULTS)

    def test_write_config_rejects_symlinked_parent(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outside = root / 'outside'
            outside.mkdir()
            redirected = root / 'config'
            redirected.symlink_to(outside, target_is_directory=True)
            with self.assertRaises(ValueError):
                governance.write_config(
                    redirected / 'config.json',
                    installer_detected_max_displays=1,
                    max_displays=None,
                    last_detected_displays=[],
                )
            self.assertFalse((outside / 'config.json').exists())


if __name__ == '__main__':
    unittest.main()
