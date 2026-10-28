"""Static native package contract; synthetic binaries are never executed."""
from pathlib import Path
import tempfile
import tomllib
import unittest
from unittest import mock
from package_native import windows_script, debian_tree


class NativeTests(unittest.TestCase):
    def test_crash_probe_commits_checks_and_closes_its_database_handles(self):
        from verify_daemon_crash import verify_database
        with tempfile.TemporaryDirectory() as temporary:
            database = Path(temporary) / 'synthetic.sqlite3'
            verify_database(database, first_start=True)
            verify_database(database, first_start=False)
            # On Windows an unclosed SQLite handle would fail with WinError 32.
            database.unlink()
            self.assertFalse(database.exists())

    def test_native_gui_fixture_uses_only_isolated_network_settings(self):
        import runpy
        fixture = runpy.run_path(str(Path(__file__).with_name('native-ui-e2e.py')))
        settings = tomllib.loads(fixture['isolated_settings'](Path('/synthetic/downloads')))
        self.assertTrue(settings['connection']['random_port_on_start'])
        self.assertFalse(settings['connection']['port_forwarding'])
        self.assertFalse(settings['connection']['utp_enabled'])
        self.assertFalse(settings['connection']['prefer_utp'])
        self.assertFalse(settings['privacy']['blocklist_auto_update'])
        self.assertFalse(any(settings['bittorrent'].values()))

    def test_both_debian_flavors_require_a_system_certificate_store(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'archive'
            root.mkdir()
            repository = Path(__file__).resolve().parent.parent
            for flavor in ('tui', 'desktop'):
                with self.subTest(flavor=flavor):
                    stage = Path(temporary) / flavor
                    debian_tree(root, stage, repository, '0.1.0', 'a' * 40, flavor)
                    control = (stage / 'DEBIAN/control').read_text()
                    depends = next(line for line in control.splitlines() if line.startswith('Depends:'))
                    self.assertIn('ca-certificates', depends.split(':', 1)[1].strip().split(', '))

    def test_install_round_trip_refuses_a_developer_machine_before_any_subprocess(self):
        import contextlib
        import io
        import os
        import sys
        import verify_native_install
        with mock.patch.dict(os.environ, {'GITHUB_ACTIONS':'false'}), mock.patch.object(sys,'argv',[
                'verify-native','--platform','windows-x64','--flavor','desktop','--commit','a'*40]), mock.patch('subprocess.run') as execute, contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as failure:
                verify_native_install.main()
            self.assertEqual(failure.exception.code,2)
            execute.assert_not_called()
    def test_windows_per_user_explicit_files_and_no_profile_autostart_or_deletion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'archive'
            root.mkdir()
            (root / 'peerlume.exe').write_bytes(b'fixture')
            script = windows_script(root, Path(temporary), 'fixture', '0.1.0', 'desktop')
            self.assertIn('PrivilegesRequired=lowest', script)
            self.assertIn('Flags: unchecked', script)
            self.assertIn('CloseApplications=no', script)
            for denied in ('recursesubdirs', '[Run]', '[Registry]', '[UninstallDelete]', 'settings.toml', 'state.sqlite3'):
                self.assertNotIn(denied, script)
            self.assertIn('PeerLume Headless', windows_script(root, Path(temporary), 'fixture', '0.1.0', 'tui'))

    def test_debian_tree_has_real_binaries_user_service_and_no_maintainer_scripts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'archive'
            (root / 'bin').mkdir(parents=True)
            (root / 'bin/peerlume-daemon').write_bytes(b'fixture')
            stage = Path(temporary) / 'debian'
            repository = Path(__file__).resolve().parent.parent
            debian_tree(root, stage, repository, '0.1.0', 'a' * 40, 'tui')
            self.assertTrue((stage / 'usr/bin/peerlume-daemon').is_file())
            self.assertTrue((stage / 'usr/lib/systemd/user/peerlume.service').is_file())
            self.assertEqual([path.name for path in (stage / 'DEBIAN').iterdir()], ['control'])
            self.assertIn('Package: peerlume-headless', (stage / 'DEBIAN/control').read_text())
            self.assertFalse((stage / 'etc').exists())


if __name__ == '__main__':
    unittest.main()
