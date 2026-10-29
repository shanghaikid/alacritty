#!/usr/bin/env python3
"""Let typing leave mouse-created empty anchors without changing manual copy mode."""
import re
import shlex
import subprocess
import sys


def install(tmux, socket):
    command = [tmux, '-S', socket]
    condition = '#{&&:#{==:#{mouse_x},},#{&&:#{@alacritty_mouse_anchor},#{==:#{selection_present},0}}}'
    root_lines = subprocess.check_output(command + ['list-keys', '-T', 'root'], text=True).splitlines()
    inherited_note = 'alacritty inherited root mouse binding'
    for table in ['copy-mode', 'copy-mode-vi']:
        saved = 'alacritty-saved-' + table
        lines = subprocess.check_output(command + ['list-keys', '-T', table], text=True).splitlines()
        # Any also matches mouse events and prevents tmux's normal fallback to
        # root. Bring over root mouse bindings that this copy table does not
        # override; execute their commands directly to retain the mouse target.
        notes = subprocess.check_output(command + ['list-keys', '-T', table, '-F',
                                                   '#{key_string}\t#{key_note}'], text=True)
        existing = dict(line.split('\t', 1) for line in notes.splitlines())
        inherited = {key for key, note in existing.items() if note == inherited_note}
        root_mouse = set()
        mouse_bindings = []
        for line in root_lines:
            match = re.search(r'-T\s+root\s+(\S+)\s+', line)
            key = shlex.split(match[1])[0]
            if not any(word in key for word in ['Mouse', 'Wheel', 'Click']):
                continue
            root_mouse.add(key)
            if key not in existing or key in inherited:
                line = re.sub(r'-T\s+root\s+', '-T ' + table + ' ', line, count=1)
                mouse_bindings.append(line.replace('bind-key',
                    'bind-key -N "' + inherited_note + '"', 1))
        for key in inherited - root_mouse:
            subprocess.run(command + ['unbind-key', '-T', table, key], check=True)
        if mouse_bindings:
            subprocess.run(command + ['source-file', '-'], input='\n'.join(mouse_bindings) + '\n', text=True, check=True)
        keys = set()
        originals = []
        for line in lines:
            match = re.search(r'-T\s+' + re.escape(table) + r'\s+(\S+)\s+', line)
            if not match:
                raise ValueError('Unrecognized tmux binding: ' + line)
            key = shlex.split(match[1])[0]
            # Mouse selection and synthetic anchor commands retain their bindings.
            # Enter keeps its existing copy-and-exit behavior.
            if any(word in key for word in ['Mouse', 'Wheel', 'Click', 'User']) or key == 'Enter':
                continue
            keys.add(key)
            if '@alacritty_mouse_anchor' not in line:
                originals.append(line[:match.start()] + line[match.start():].replace(table, saved, 1))
        if originals:
            subprocess.run(command + ['source-file', '-'], input='\n'.join(originals) + '\n', text=True, check=True)
        keys.add('Any')  # Includes unbound printable characters and UTF-8 input.
        saved_keys = subprocess.check_output(command + ['list-keys', '-T', saved, '-F', '#{key_string}'], text=True).splitlines()
        pending = []
        for key in sorted(keys):
            args = ['bind-key', '-T', table, '\\;' if key == ';' else key, 'if-shell', '-F', condition,
                              'send-keys -X cancel; send-keys']
            if key in saved_keys:
                # Replay the original key in its saved table. Unlike serializing
                # command strings again, this preserves quoting and custom bindings.
                args.append('switch-client -T ' + saved + '; send-keys -K')
            pending.append(args)
        # Loading this file is on the terminal startup path. Send bounded batches
        # through one client per batch instead of spawning a client per key.
        for start in range(0, len(pending), 64):
            batch = []
            for args in pending[start:start + 64]:
                if batch:
                    batch.append(';')
                batch.extend(args)
            subprocess.run(command + batch, check=True)


if __name__ == '__main__':
    install(sys.argv[1], sys.argv[2])
