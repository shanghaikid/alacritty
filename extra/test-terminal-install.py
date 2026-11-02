#!/usr/bin/env python3
"""Exercise installation, upgrades and a fresh tmux server without user config."""

import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import signal
import sys
import subprocess
import tempfile
import time
import tomllib
import unittest

EXTRA = Path(__file__).resolve().parent
sys.path.insert(0, str(EXTRA / 'terminal-profile'))
spec = importlib.util.spec_from_file_location('terminal_install', EXTRA / 'terminal-profile/install.py')
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
TMUX = Path(os.environ.get('TEST_TMUX', Path.home() / '.local/bin/tmux'))


class Installation(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='alacritty-install-test-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home with space's"
        self.home.mkdir()
        self.app = self.root / 'Applications with spaces/Alacritty.app'
        source = self.app / 'Contents/Resources/terminal-profile'
        shutil.copytree(EXTRA / 'terminal-profile', source)
        for name in ('merino-light.toml', 'merino-dark.toml', 'rex-appearance.toml',
                     'merino-light-tmux.conf', 'merino-dark-tmux.conf',
                     'tmux-shift-selection.conf', 'tmux-mouse-anchor-input.py'):
            shutil.copy2(EXTRA / name, source / name)
        (self.app / 'Contents/MacOS').mkdir()
        (self.app / 'Contents/MacOS/tmux').symlink_to(TMUX)
        self.config = self.home / '.config/alacritty'
        self.profile = self.config / 'terminal-profile'

    def install(self):
        return installer.install_profile(self.app, self.home, self.config, self.home, self.home / '.codex')

    def test_fresh_and_repeat(self):
        self.install()
        before = {p: p.read_bytes() for p in self.home.rglob('*') if p.is_file()}
        self.install()
        after = {p: p.read_bytes() for p in self.home.rglob('*') if p.is_file()}
        self.assertEqual(before, after)
        config = tomllib.loads((self.config / 'alacritty.toml').read_text())
        self.assertEqual(config['terminal']['shell'], str(self.profile / 'start.zsh'))
        for path in config['general']['import']:
            tomllib.loads(Path(path).read_text())
        env = dict(os.environ, HOME=str(self.home), ZDOTDIR=str(self.home),
                   ALACRITTY_TERMINAL_PROFILE='merino')
        result = subprocess.run(['/bin/zsh', '-ic', 'print -r -- "$PROMPT|$RPROMPT"'],
                                env=env, capture_output=True, text=True, check=True)
        self.assertIn('%F{blue}%~', result.stdout)
        self.assertIn('%F{8}%*', result.stdout)
        env.pop('ALACRITTY_TERMINAL_PROFILE')
        result = subprocess.run(['/bin/zsh', '-ic', 'print -r -- "$PROMPT"'],
                                env=env, capture_output=True, text=True, check=True)
        self.assertNotIn('%F{blue}%~', result.stdout)

    def test_existing_symlinks_hooks_and_backup(self):
        original = self.root / 'user-zshrc'
        original.write_text('export MY_SETTING=retained\n')
        (self.home / '.zshrc').symlink_to(original)
        self.config.mkdir(parents=True)
        (self.config / 'alacritty.toml').write_text('[font]\nsize = 17\n')
        hooks = self.home / '.codex/hooks.json'
        hooks.parent.mkdir()
        hooks.write_text(json.dumps({'hooks': {'Stop': [{'hooks': [
            {'type': 'command', 'command': 'my-existing-hook'}]}]}, 'custom': True}))
        writer = self.install()
        self.assertEqual(original.read_text(), 'export MY_SETTING=retained\n')
        self.assertIn('export MY_SETTING=retained', (self.home / '.zshrc').read_text())
        self.assertTrue((writer.backup / str(self.home / '.zshrc').lstrip('/')).is_symlink())
        updated = json.loads(hooks.read_text())
        self.assertTrue(updated['custom'])
        self.assertEqual(updated['hooks']['Stop'][0]['hooks'][0]['command'], 'my-existing-hook')
        self.install()
        self.assertEqual(json.loads(hooks.read_text()), updated)
        self.assertFalse((self.home / '.codex/config.toml').exists())

    def test_invalid_input_does_not_change_configuration(self):
        (self.home / '.zshrc').write_text(installer.BEGIN)
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse(self.config.exists())

    def test_incremental_install_keeps_dotfiles_links(self):
        spec = importlib.util.spec_from_file_location('theme_upgrade', EXTRA / 'install-merino-toggle.py')
        upgrade = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(upgrade)
        dotfiles = self.home / 'dotfiles'
        dotfiles.mkdir()
        original = dotfiles / 'alacritty.toml'
        original.write_text('[font]\nsize = 15\n')
        self.config.mkdir(parents=True)
        (self.config / 'alacritty.toml').symlink_to(original)
        for name in ('.zshrc', '.tmux.conf'):
            (dotfiles / name).write_text('# user configuration\n')
            (self.home / name).symlink_to(dotfiles / name)
        upgrade.install(self.home, TMUX)
        upgrade.install(self.home, TMUX)
        self.assertEqual((self.config / 'alacritty-before-theme.toml').resolve(), original.resolve())
        self.assertEqual(original.read_text(), '[font]\nsize = 15\n')
        for name in ('.zshrc', '.tmux.conf'):
            self.assertTrue((self.home / name).is_symlink())
            self.assertEqual((dotfiles / name).read_text(), '# user configuration\n')
        self.assertEqual((self.config / 'tmux.conf').read_text().count('theme.conf'), 1)

    @unittest.skipUnless(TMUX.exists(), 'TEST_TMUX must point to the OSC 7501 tmux build')
    def test_theme_switch_updates_tmux_and_survives_install(self):
        self.install()
        socket = self.root / 'theme-socket'
        command = [str(TMUX), '-S', str(socket)]
        env = dict(os.environ, HOME=str(self.home), ZDOTDIR=str(self.home))
        env.pop('TMUX', None)
        rc = (self.home / '.zshrc').read_bytes()
        toggle = tomllib.loads((self.config / 'alacritty.toml').read_text())['window']['theme_toggle_command']
        try:
            subprocess.run(command + ['-f', str(self.profile / 'tmux.conf'),
                                      'new-session', '-d', '-s', 'test'], env=env, check=True)
            deadline = time.monotonic() + 5
            while not (self.profile / 'servers.json').exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.02)
            for background, appearance in (('#0d1117', 'Dark'), ('#ffffff', 'Light')):
                subprocess.run([toggle['program']] + toggle['args'], check=True, capture_output=True)
                active = tomllib.loads((self.profile / 'active-theme.toml').read_text())
                self.assertEqual(active['colors']['primary']['background'], background)
                self.assertEqual(active['window']['decorations_theme_variant'], appearance)
                style = subprocess.check_output(command + ['show-options', '-gwv', 'window-style'], text=True)
                self.assertIn(background, style)
                self.install()
                self.assertEqual(tomllib.loads((self.profile / 'active-theme.toml').read_text()), active)
                self.assertEqual((self.home / '.zshrc').read_bytes(), rc)
        finally:
            subprocess.run(command + ['kill-server'], env=env, capture_output=True)

    @unittest.skipUnless(TMUX.exists(), 'TEST_TMUX must point to the OSC 7501 tmux build')
    def test_installed_launcher_reconnects(self):
        self.install()
        socket = str(self.root / 'launcher-socket')
        start = self.profile / 'start.zsh'
        start.write_text(start.read_text().replace('-L alacritty-merino', '-S ' + shlex.quote(socket)))
        env = dict(os.environ, HOME=str(self.home), ZDOTDIR=str(self.home), TERM='alacritty')
        env.pop('TMUX', None)
        command = [str(TMUX), '-S', socket]

        def tmux(*args):
            return subprocess.check_output(command + list(args), env=env, text=True,
                                           stderr=subprocess.PIPE, timeout=10).strip()

        def attach():
            pid, master = pty.fork()
            if pid == 0:
                os.chdir(self.home)
                os.execve(str(start), [str(start)], env)
            try:
                output = b''
                deadline = time.monotonic() + 10
                while b'\xe2\x9d\xaf' not in output:  # The configured zsh prompt glyph.
                    self.assertLess(time.monotonic(), deadline, output[-1000:])
                    if select.select([master], [], [], .1)[0]:
                        output += os.read(master, 65536)
                os.write(master, b'\x02d')
                deadline = time.monotonic() + 5
                while os.waitpid(pid, os.WNOHANG)[0] == 0:
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                pid = None
            finally:
                if pid is not None:
                    try:
                        os.kill(pid, signal.SIGHUP)
                    except ProcessLookupError:
                        pass
                    os.waitpid(pid, 0)
                os.close(master)

        try:
            attach()
            tmux('new-window', '-d', '-t', '=alacritty')
            before = tmux('list-windows', '-t', '=alacritty', '-F', '#{window_id}')
            bindings = tmux('list-keys')
            attach()
            self.assertEqual(tmux('list-windows', '-t', '=alacritty', '-F', '#{window_id}'), before)
            self.assertEqual(tmux('list-keys'), bindings)
        finally:
            subprocess.run(command + ['kill-server'], env=env, capture_output=True)

    @unittest.skipUnless(TMUX.exists(), 'TEST_TMUX must point to the OSC 7501 tmux build')
    def test_real_tmux_in_fresh_home(self):
        self.install()
        env = dict(os.environ, HOME=str(self.home), ZDOTDIR=str(self.home),
                   PATH='/opt/homebrew/bin:/usr/bin:/bin', TERM='alacritty')
        env.pop('TMUX', None)
        command = [str(TMUX), '-S', str(self.root / 'socket')]

        def tmux(*args):
            result = subprocess.run(command + list(args), env=env, cwd=self.home,
                                    text=True, capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout.strip()

        try:
            tmux('-f', str(self.profile / 'tmux.conf'), 'new-session', '-d', '-s', 'test')
            errors = tmux('show-messages')
            self.assertNotIn('error', errors.lower())
            self.assertEqual(tmux('show-options', '-gv', 'status-position'), 'top')
            self.assertIn('#eceef2', tmux('show-options', '-gv', 'status-style'))
            self.assertIn('#ffffff', tmux('show-options', '-gwv', 'window-style'))
            self.assertEqual(tmux('show-environment', '-g', 'ALACRITTY_TERMINAL_PROFILE'),
                             'ALACRITTY_TERMINAL_PROFILE=merino')
            bindings = tmux('list-keys', '-T', 'copy-mode-vi')
            self.assertIn('@alacritty_mouse_anchor', bindings)
            self.assertIn('MouseDragEnd1Pane', bindings)
            # Execute inside the fresh pane to check startup files and status parsing.
            tmux('send-keys', '-l', "printf '\\033]7501;state=working:id=test:app=test\\033\\\\'; print -r -- \"PROFILE=$ALACRITTY_TERMINAL_PROFILE\"")
            tmux('send-keys', 'Enter')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if tmux('display-message', '-p', '#{pane_program_status}'):
                    break
                time.sleep(.05)
            self.assertTrue(tmux('display-message', '-p', '#{pane_program_status}'))
            self.assertIn('PROFILE=merino', tmux('capture-pane', '-p'))
            tmux('source-file', str(self.profile / 'tmux.conf'))
            self.assertEqual(tmux('list-keys', '-T', 'copy-mode-vi'), bindings)
        finally:
            subprocess.run(command + ['kill-server'], env=env, capture_output=True)


if __name__ == '__main__':
    unittest.main()
