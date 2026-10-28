#!/usr/bin/env python3
"""Let typing leave mouse-created empty anchors without changing manual copy mode."""
import re
import shlex
import subprocess
import sys


def install(tmux, socket):
    command = [tmux, '-S', socket]
    condition = '#{&&:#{==:#{mouse_x},},#{&&:#{@alacritty_mouse_anchor},#{==:#{selection_present},0}}}'
    for table in ['copy-mode', 'copy-mode-vi']:
        saved = 'alacritty-saved-' + table
        lines = subprocess.check_output(command + ['list-keys', '-T', table], text=True).splitlines()
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
        for key in sorted(keys):
            args = command + ['bind-key', '-T', table, '\\;' if key == ';' else key, 'if-shell', '-F', condition,
                              'send-keys -X cancel; send-keys']
            if key in saved_keys:
                # Replay the original key in its saved table. Unlike serializing
                # command strings again, this preserves quoting and custom bindings.
                args.append('switch-client -T ' + saved + '; send-keys -K')
            subprocess.run(args, check=True)


if __name__ == '__main__':
    install(sys.argv[1], sys.argv[2])
