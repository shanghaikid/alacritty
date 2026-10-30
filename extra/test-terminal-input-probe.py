#!/usr/bin/env python3
"""Exercise input-probe failure handling without opening windows or compiling."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "input_probe", Path(__file__).with_name("compare-terminal-input.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class InputProbeTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="terminal-input-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.binary = self.root / "terminal"
        self.binary.write_bytes(b"test executable")
        self.args = SimpleNamespace(terminal="alacritty", label="run", output=str(self.root),
                                    binary=str(self.binary))
        self.run_directory = self.root / "run"

    def compile_probe(self, command, **kwargs):
        (self.run_directory / "probe.dylib").write_bytes(b"test library")
        return subprocess.CompletedProcess(command, 0)

    def terminal(self, complete, error=False):
        def launch(command, **kwargs):
            kwargs["stdout"].write("input probe installed\n")
            kwargs["stdout"].flush()
            (self.run_directory / "samples.csv").write_text("1.0\n" * 100)
            if complete:
                (self.run_directory / "ready.complete").write_text("100")
            if error:
                (self.run_directory / "ready.error").write_text("child failed after sampling")
            # A terminal may exit successfully even when its PTY child failed.
            return SimpleNamespace(returncode=0, poll=lambda: 0)
        return launch

    def check_run(self, complete, error=False):
        with patch.object(probe.subprocess, "run", side_effect=self.compile_probe), \
             patch.object(probe.subprocess, "Popen", side_effect=self.terminal(complete, error)), \
             contextlib.redirect_stdout(io.StringIO()):
            if complete and not error:
                probe.main(self.args)
            else:
                with self.assertRaises(AssertionError):
                    probe.main(self.args)
        result = json.loads((self.run_directory / "results.json").read_text())
        self.assertEqual(result["valid"], complete and not error)
        self.assertFalse((self.run_directory / "probe.dylib").exists())

    def test_samples_and_terminal_success_do_not_replace_child_completion(self):
        self.check_run(complete=False)

    def test_explicit_child_completion_accepts_the_run(self):
        self.check_run(complete=True)

    def test_child_error_rejects_even_a_completed_sample_set(self):
        self.check_run(complete=True, error=True)

    def test_signing_failure_removes_compiled_probe(self):
        def compile_then_fail(command, **kwargs):
            if command[0] == "codesign":
                raise subprocess.CalledProcessError(1, command)
            return self.compile_probe(command, **kwargs)
        with patch.object(probe.subprocess, "run", side_effect=compile_then_fail):
            with self.assertRaises(subprocess.CalledProcessError):
                probe.main(self.args)
        self.assertFalse((self.run_directory / "probe.dylib").exists())


if __name__ == "__main__":
    unittest.main()
