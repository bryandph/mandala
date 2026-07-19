import tempfile
import unittest
from pathlib import Path

from ansible_collections.mandala.fleet.plugins.module_utils.events import Emitter


class EmitterPathSafetyTests(unittest.TestCase):
    def test_accepts_bare_rfc1123_hostname(self):
        with tempfile.TemporaryDirectory() as directory:
            emitter = Emitter(directory, "web-1", "deploy")
            emitter.status("start")
            self.assertTrue((Path(directory) / "web-1.jsonl").is_file())
            emitter._fh.close()

    def test_rejects_names_that_can_escape_or_are_fqdns(self):
        with tempfile.TemporaryDirectory() as directory:
            for host in ("../escape", "/tmp/escape", "web.example.test", "web_node", "-web", "web-"):
                with self.subTest(host=host), self.assertRaises(ValueError):
                    Emitter(directory, host, "deploy")


if __name__ == "__main__":
    unittest.main()
