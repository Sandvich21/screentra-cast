"""Non-destructive installer contracts: only isolated definitions are executed."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PAYLOAD = ROOT / 'static/install/player-client.sh'
HELPER = ROOT / 'iso/installer-contract.sh'


def shell(code, env=None):
    return subprocess.run(['bash', '-euc', code], text=True, capture_output=True,
                          env=env or os.environ.copy())


class InstallerContract(unittest.TestCase):
    def helper(self, code):
        self.assertTrue(HELPER.exists(), 'isolated installer contract helper missing')
        return shell(f'source "{HELPER}"; ' + code)

    def test_payload_absent_home_and_root_paths(self):
        prefix = PAYLOAD.read_text().split('is_ssh_session() {', 1)[0]
        env = os.environ.copy()
        for key in ('HOME', 'USER', 'LOGNAME'):
            env.pop(key, None)
        result = shell(prefix + '\nprintf "%s\\n" "$HOME" "$BIN_DIR" "$SYSTEMD_USER_DIR"', env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines()[-2:],
                         ['/home/board/.local/bin', '/home/board/.config/systemd/user'])

    def test_caller_home_not_runtime_home(self):
        prefix = PAYLOAD.read_text().split('is_ssh_session() {', 1)[0]
        result = shell('HOME=/tmp/caller; ' + prefix + '\nprintf "%s" "$BIN_DIR"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '/home/board/.local/bin')

    def test_runtime_paths_use_passwd_home_for_root_and_nonroot(self):
        prefix = PAYLOAD.read_text().split('is_ssh_session() {', 1)[0]
        for uid, user in [('0', 'root'), ('1002', 'operator')]:
            mock = f'''id() {{ if [ "$1" = -u ]; then echo {uid}; else echo {user}; fi; }}
getent() {{ if [ "$2" = board ]; then echo board:x:1001:1001::/srv/board:/bin/bash; else echo {user}:x:{uid}:{uid}::/home/{user}:/bin/bash; fi; }}
unset HOME USER LOGNAME
'''
            r = shell(mock + prefix + '\nprintf "%s\\n" "$HOME" "$BIN_DIR" "$SYSTEMD_USER_DIR"')
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.splitlines(), [f'/home/{user}', '/srv/board/.local/bin', '/srv/board/.config/systemd/user'])

    def test_runtime_rejects_unsafe_inputs_before_mutation(self):
        prefix = PAYLOAD.read_text().split('is_ssh_session() {', 1)[0]
        cases = [
            ('board', '/home/board', '0', ''),
            ('root', '/root', '0', ''),
            ('../root', '/home/../root', '1001', ''),
            ('board', '/', '1001', ''),
            ('board', '/etc', '1001', ''),
            ('board', '', '1001', ''),
            ('board', '/home/other', '1001', ''),
            ('board', '/home/board', '1001', '/'),
            ('board', '/home/board', '1001', '/etc'),
            ('board', '/home/board', '1001', '/home/board/../../etc'),
        ]
        for user, home, uid, destination in cases:
            with self.subTest(user=user, home=home, destination=destination):
                env = os.environ.copy()
                env.update(SCREENTRA_BOARD_USER=user, SCREENTRA_CLIENT_DIR=destination)
                mock = f'getent() {{ printf "%s\\n" "{user}:x:{uid}:1001::{home}:/bin/bash"; }}\n'
                r = shell(mock + prefix + '\nprintf MUTATION_REACHED', env)
                self.assertNotEqual(r.returncode, 0, r.stdout)
                self.assertNotIn('MUTATION_REACHED', r.stdout)

    def test_runtime_lookup_failure_does_not_create_account(self):
        prefix = PAYLOAD.read_text().split('is_ssh_session() {', 1)[0]
        r = shell('getent() { return 73; };\n' + prefix + '\nprintf MUTATION_REACHED')
        self.assertNotEqual(r.returncode, 0)
        self.assertNotIn('MUTATION_REACHED', r.stdout)

    def test_runtime_directory_rejects_symlink_escapes(self):
        text = PAYLOAD.read_text()
        self.assertTrue('validate_runtime_directory() {' in text, 'runtime path guard missing')
        fn = 'validate_runtime_directory() {' + text.split('validate_runtime_directory() {', 1)[1].split('\n}', 1)[0] + '\n}'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root/'safe').mkdir()
            (root/'escape').symlink_to(root/'safe', target_is_directory=True)
            (root/'safe'/'file-link').symlink_to('/etc/passwd')
            for path in (root/'escape'/'new', root/'safe'):
                r = shell('runtime_path_error() { exit 1; }; ' + fn + f'\nvalidate_runtime_directory "{path}"')
                self.assertNotEqual(r.returncode, 0)
            (root/'safe'/'file-link').unlink()
            r = shell('runtime_path_error() { exit 1; }; ' + fn + f'\nvalidate_runtime_directory "{root}/safe/new"')
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_runtime_allows_generated_command_and_unit_links(self):
        text = PAYLOAD.read_text()
        fn = 'validate_runtime_directory() {' + text.split('validate_runtime_directory() {', 1)[1].split('\n}', 1)[0] + '\n}'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for directory in ('payload', 'bin', 'units', 'units/default.target.wants'):
                (root/directory).mkdir()
            (root/'payload/helper').write_text('fixture')
            (root/'bin/helper').symlink_to(root/'payload/helper')
            (root/'units/helper.service').write_text('fixture')
            (root/'units/default.target.wants/helper.service').symlink_to('../helper.service')
            r = shell(f'INSTALL_DIR="{root}/payload"; SYSTEMD_USER_DIR="{root}/units"; runtime_path_error() {{ exit 1; }}; ' + fn +
                      f'\nvalidate_runtime_directory "{root}/bin"; validate_runtime_directory "{root}/units"')
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_url_rejects_secrets_and_injection(self):
        for url in ('https://user:secret@example.org', 'https://example.org/?token=secret',
                    'https://example.org/#secret', 'https://example.org/"', 'not-a-url'):
            self.assertNotEqual(self.helper('validate_url ' + __import__('shlex').quote(url)).returncode, 0)
        self.assertEqual(self.helper('validate_url https://example.org:3000').returncode, 0)

    def test_network_mapping(self):
        for mode, expected in [('dhcp', 'auto'), ('static', 'manual')]:
            r = self.helper(f'nm_method {mode}')
            self.assertEqual((r.returncode, r.stdout.strip()), (0, expected))
        r = self.helper('nm_dns "1.1.1.1 8.8.8.8"')
        self.assertEqual(r.stdout.strip(), '1.1.1.1;8.8.8.8;')

    def test_validation_rejects_bad_inputs(self):
        for args in ['bad_name dhcp "" "" ""',
                     'client static 192.168.1.3/99 192.168.1.1 1.1.1.1',
                     'client static 192.168.1.3/24 nope 1.1.1.1',
                     'client static 192.168.1.3/24 192.168.1.1 "1.1.1.1;evil"']:
            self.assertNotEqual(self.helper('validate_network ' + args).returncode, 0)
        self.assertEqual(self.helper('validate_network client static 192.168.1.3/24 192.168.1.1 "1.1.1.1 8.8.8.8"').returncode, 0)

    def test_resolver_mode_under_restrictive_umask(self):
        with tempfile.TemporaryDirectory() as tmp:
            src, dst = Path(tmp)/'source', Path(tmp)/'resolver'
            src.write_text('nameserver 1.1.1.1\n')
            r = self.helper(f'umask 077; install_resolver "{src}" "{dst}"')
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(dst.stat().st_mode & 0o777, 0o644)

    def test_offline_systemctl_never_contacts_bus(self):
        text = PAYLOAD.read_text()
        self.assertIn('player_systemctl() {', text)
        fn = text.split('# BEGIN SETUP SERVICE HELPER\n')[1].split('# END SETUP SERVICE HELPER')[0]
        r = shell('SCREENTRA_INSTALL_MODE=offline; run_privileged() { printf "CALL:%s\\n" "$*"; }; ' + fn + '\nplayer_systemctl daemon-reload; player_systemctl restart foo; player_systemctl enable --now foo')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.strip(), 'CALL:systemctl --root=/ enable foo')
        r = shell('SCREENTRA_INSTALL_MODE=online; run_privileged() { printf "CALL:%s\\n" "$*"; }; ' + fn + '\nplayer_systemctl enable --now foo')
        self.assertEqual(r.stdout.strip(), 'CALL:systemctl enable --now foo')

    def test_completion_missing_artifacts_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            r = self.helper(f'verify_boot_files "{tmp}"')
            self.assertNotEqual(r.returncode, 0)
            for path in ['boot/vmlinuz-test', 'boot/initrd.img-test', 'boot/grub/grub.cfg',
                         'boot/efi/EFI/Screentra Cast/grubx64.efi', 'boot/efi/EFI/BOOT/BOOTX64.EFI']:
                p = Path(tmp)/path
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text('fixture')
            self.assertEqual(self.helper(f'verify_boot_files "{tmp}"').returncode, 0)
            (Path(tmp)/'boot/efi/EFI/BOOT/BOOTX64.EFI').unlink()
            self.assertNotEqual(self.helper(f'verify_boot_files "{tmp}"').returncode, 0)

    def test_completion_gate_with_mocked_target_commands(self):
        source = (ROOT/'iso/client-install.sh').read_text()
        verification = source.split('stage verification\n', 1)[1].split('stage completed', 1)[0]
        for failure in ('package', 'sshd', 'enable', 'ownership', 'find-error', 'boot', ''):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                for path in ['boot/vmlinuz-test', 'boot/initrd.img-test', 'boot/grub/grub.cfg',
                             'boot/efi/EFI/Screentra Cast/grubx64.efi', 'boot/efi/EFI/BOOT/BOOTX64.EFI']:
                    p = Path(tmp)/path
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_text('fixture')
                if failure == 'boot':
                    (Path(tmp)/'boot/grub/grub.cfg').unlink()
                mock = '''
PACKAGES=(fixture-package)
server_url=https://example.org
die() { exit 1; }
sync() { :; }
target_exec() {
    case "$1" in
        dpkg-query) [ "$FAIL" != package ] && printf 'install ok installed';;
        /usr/sbin/sshd) [ "$FAIL" != sshd ];;
        systemctl) [ "$FAIL" != enable ];;
        getent) printf 'board:x:1000:1000::/home/board:/bin/bash\\n';;
        find) if [ "$FAIL" = find-error ]; then return 73; fi; if [ "$FAIL" = ownership ]; then printf bad-owner; fi;;
        runuser) return 0;;
        *) return 99;;
    esac
}
'''
                r = self.helper(f'TARGET="{tmp}"; FAIL="{failure}"; ' + mock + verification)
                marker = Path(tmp)/'etc/screentra-cast/install-complete'
                if failure:
                    self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
                    self.assertFalse(marker.exists())
                else:
                    self.assertEqual(r.returncode, 0, r.stderr)
                    self.assertTrue(marker.exists())

    def test_offline_bus_and_seat_helpers_skip_probes(self):
        text = PAYLOAD.read_text()
        for name in ('ensure_dbus', 'ensure_seat_daemon'):
            fn = name + '() {' + text.split(name + '() {', 1)[1].split('\n}', 1)[0] + '\n}'
            r = shell('SCREENTRA_INSTALL_MODE=offline; ' + fn + '\n' + name)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout, '')
        self.assertLess(text.index('if [ "$SCREENTRA_INSTALL_MODE" = offline ]; then\n    echo "Offline'),
                        text.index('"$HEARTBEAT" --once'))

    def test_policy_restore_preserves_original(self):
        text = (ROOT/'iso/client-install.sh').read_text()
        fn = 'restore_policy() {' + text.split('restore_policy() {', 1)[1].split('\n}', 1)[0] + '\n}'
        for original in (False, True):
            with tempfile.TemporaryDirectory() as tmp:
                p = Path(tmp)/'usr/sbin'
                p.mkdir(parents=True)
                (p/'policy-rc.d').write_text('temporary')
                if original:
                    (p/'policy-rc.d.screentra-original').symlink_to('/original-policy')
                r = shell(f'TARGET="{tmp}"; POLICY_ACTIVE=1; ' + fn + '\nrestore_policy')
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual((p/'policy-rc.d').is_symlink(), original)
                if original:
                    self.assertEqual(os.readlink(p/'policy-rc.d'), '/original-policy')
                else:
                    self.assertFalse((p/'policy-rc.d').exists())

    def test_policy_restore_failure_is_retryable(self):
        text = (ROOT/'iso/client-install.sh').read_text()
        fn = 'restore_policy() {' + text.split('restore_policy() {', 1)[1].split('\n}', 1)[0] + '\n}'
        for command in ('rm', 'mv'):
            with self.subTest(command=command), tempfile.TemporaryDirectory() as tmp:
                p = Path(tmp)/'usr/sbin'
                p.mkdir(parents=True)
                (p/'policy-rc.d').write_text('temporary')
                (p/'policy-rc.d.screentra-original').write_text('original')
                r = shell(f'TARGET="{tmp}"; POLICY_ACTIVE=1; ' + fn +
                          f'\n{command}() {{ return 73; }}\nset +e\nrestore_policy\nrc=$?\nprintf "%s %s" "$rc" "$POLICY_ACTIVE"')
                self.assertEqual(r.stdout, '73 1')
                self.assertIn('policy', r.stderr)
                self.assertEqual((p/'policy-rc.d.screentra-original').read_text(), 'original')
                if command == 'rm':
                    self.assertEqual((p/'policy-rc.d').read_text(), 'temporary')
                retry = shell(f'TARGET="{tmp}"; POLICY_ACTIVE=1; ' + fn + '\nrestore_policy; printf "%s" "$POLICY_ACTIVE"')
                self.assertEqual((retry.returncode, retry.stdout), (0, '0'))
                self.assertEqual((p/'policy-rc.d').read_text(), 'original')

    def test_disk_refusal_and_exact_confirmation(self):
        text = (ROOT/'iso/client-install.sh').read_text()
        checks = text.split('while read -r parent; do', 1)[1].split('\nstage partition', 1)[0]
        checks = 'while read -r parent; do' + checks
        for failure in ('live', 'mounted', 'swap', 'raid', 'confirmation', ''):
            mock = '''
DISK=/dev/test-target
live_ancestry=/dev/live
[ "$FAIL" != live ] || live_ancestry=$DISK
SCREENTRA_ERASE_CONFIRM="ERASE $DISK"
[ "$FAIL" != confirmation ] || SCREENTRA_ERASE_CONFIRM="ERASE /dev/other"
die() { echo "$*" >&2; exit 1; }
cancel() { exit 2; }
readlink() { printf '%s\\n' "${@: -1}"; }
findmnt() { [ "$FAIL" = mounted ]; }
swapon() { if [ "$FAIL" = swap ]; then echo /dev/test-target1; fi; }
lsblk() {
    echo '/dev/test-target disk'
    echo '/dev/test-target1 part'
    if [ "$FAIL" = raid ]; then echo '/dev/dm-0 lvm'; fi
}
'''
            r = shell(f'FAIL={failure};\n' + mock + checks + '\nprintf PARTITION_REACHED')
            with self.subTest(failure=failure):
                self.assertEqual(r.returncode == 0, not failure, r.stderr)
                self.assertEqual('PARTITION_REACHED' in r.stdout, not failure)
        selection = text.split('DISK=${SCREENTRA_TARGET_DISK:-}', 1)[1].split('# Fail closed', 1)[0]
        r = shell('DISK=/dev/null; die() { exit 1; }; cancel() { exit 2; };\n' + selection)
        self.assertNotEqual(r.returncode, 0)

    def test_cleanup_reverse_order_and_failure_log(self):
        text = (ROOT/'iso/client-install.sh').read_text()
        fn = 'finish() {' + text.split('finish() {', 1)[1].split('\n}', 1)[0] + '\n}'
        for initial, cleanup_failure in ((73, False), (0, True)):
            with tempfile.TemporaryDirectory() as tmp:
                log = Path(tmp)/'install.log'
                code = f'''
LOG="{log}"
STAGE=packages
TARGET_MOUNTED=0
LOG_PID=0
OWNED_MOUNTS=(/fake/root /fake/root/dev /fake/root/run)
restore_policy() {{ :; }}
wait() {{ :; }}
umount() {{ printf '%s\\n' "$2" >> "{tmp}/unmounted"; {'return 73' if cleanup_failure else ':'}; }}
touch() {{ echo UNEXPECTED_SUCCESS; return 99; }}
exec 3>&1 4>&2
exec >>"$LOG" 2>&1
set +e
(exit {initial})
finish
'''
                r = shell(fn + code)
                self.assertNotEqual(r.returncode, 0)
                self.assertEqual((Path(tmp)/'unmounted').read_text().splitlines(),
                                 ['/fake/root/run', '/fake/root/dev', '/fake/root'])
                self.assertIn('FAILED at packages', r.stderr)
                self.assertIn(str(log), r.stderr)
                self.assertNotIn('UNEXPECTED_SUCCESS', r.stdout)
                self.assertIn(f'stage=packages exit={initial}', log.read_text())
                if cleanup_failure:
                    self.assertIn('Cleanup failed:', log.read_text())

    def test_installer_safety_wiring(self):
        s = (ROOT/'iso/client-install.sh').read_text()
        self.assertNotIn('DISK=${DISKS[0]', s)
        self.assertIn('SCREENTRA_ERASE_CONFIRM', s)
        self.assertIn('HOME=/root USER=root LOGNAME=root', s)
        self.assertIn('APT::Update::Error-Mode=any', s)
        self.assertNotIn('screentra-cast-board-password', s)
        self.assertNotIn('dev dev/pts proc sys run', s)
        self.assertIn('verify_boot_files "$TARGET"', s)
        self.assertLess(s.index('verify_boot_files "$TARGET"'), s.rindex('"$TARGET/etc/screentra-cast/install-complete"'))
        self.assertIn('runuser -u _apt', s)


if __name__ == '__main__':
    unittest.main()
