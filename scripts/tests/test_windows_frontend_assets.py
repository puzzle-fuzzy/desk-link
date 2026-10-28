from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "verify-windows-release.py"
    spec = importlib.util.spec_from_file_location("windows_frontend_assets_release", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WindowsFrontendAssetsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.verify = load_script()

    def verify_assets(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            windows_ui = Path(directory)
            for relative, content in files.items():
                path = windows_ui / "dist" / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            with patch.object(self.verify, "WINDOWS_UI", windows_ui):
                return self.verify.verify_frontend_assets()

    @staticmethod
    def application(**extra: str) -> dict[str, str]:
        return {
            "index.html": '<script type="module" src="/assets/main.js"></script><link rel="stylesheet" href="/assets/main.css">',
            "assets/main.js": 'console.log("ready");',
            "assets/main.css": "body { color: black; }",
            **extra,
        }

    def test_accepts_direct_script_and_stylesheet_entries(self):
        self.assertEqual(self.verify_assets(self.application()), ["assets/main.css", "assets/main.js"])

    def test_accepts_bundle_to_url_loader_to_audio_worklet(self):
        files = self.application()
        files["assets/main.js"] = 'const module = await import(`./loader.js`); context.audioWorklet.addModule(module.default);'
        files["assets/loader.js"] = 'const url = `/assets/worklet.js`; export { url as default };'
        files["assets/worklet.js"] = 'registerProcessor("processor", class {});'
        self.assertEqual(self.verify_assets(files), sorted(path for path in files if path != "index.html"))

    def test_resolves_nested_relative_paths_queries_and_cycles(self):
        files = self.application()
        files["assets/main.js"] = 'import "./nested/chunk.js?v=1#module";'
        files["assets/nested/chunk.js"] = 'import "../main.js";'
        files["assets/main.css"] = '@import url(./nested/theme.css?v=1);'
        files["assets/nested/theme.css"] = "body { margin: 0; }"
        self.assertEqual(self.verify_assets(files), sorted(path for path in files if path != "index.html"))

    def test_requires_a_script_entry_even_when_modulepreload_exists(self):
        for index in ('<link rel="stylesheet" href="/assets/main.css">', '<link rel="modulepreload" href="/assets/main.js">'):
            with self.subTest(index=index):
                files = self.application()
                files["index.html"] = index
                with self.assertRaisesRegex(SystemExit, "no local JavaScript entry"):
                    self.verify_assets(files)

    def test_rejects_orphan_assets(self):
        for extension in ("js", "css"):
            with self.subTest(extension=extension):
                files = self.application(**{f"assets/orphan.{extension}": "/* unused */"})
                with self.assertRaisesRegex(SystemExit, "not reachable.*orphan"):
                    self.verify_assets(files)

    def test_rejects_missing_direct_entry(self):
        files = self.application()
        files["index.html"] += '<script src="/assets/missing.js"></script>'
        with self.assertRaisesRegex(SystemExit, "references missing local asset /assets/missing.js"):
            self.verify_assets(files)

    def test_rejects_missing_transitive_js_and_css_references(self):
        for owner, source in (
            ("assets/main.js", 'import "./missing.js";'),
            ("assets/main.js", 'export default "/assets/missing.js";'),
            ("assets/main.css", '@import url(./missing.css);'),
            ("assets/main.css", '@import "./missing.css";'),
        ):
            with self.subTest(owner=owner, source=source):
                files = self.application()
                files[owner] = source
                with self.assertRaisesRegex(SystemExit, "references missing local asset"):
                    self.verify_assets(files)

    def test_scans_development_urls_in_transitive_and_orphan_assets(self):
        for referenced in (True, False):
            with self.subTest(referenced=referenced):
                files = self.application(**{"assets/worker.js": 'fetch("http://127.0.0.1:1420/api")'})
                if referenced:
                    files["assets/main.js"] = 'new Worker("./worker.js");'
                with self.assertRaisesRegex(SystemExit, "contains development URL"):
                    self.verify_assets(files)

    def test_rejects_path_outside_distribution(self):
        files = self.application()
        files["assets/main.js"] = 'import "../../outside.js";'
        with self.assertRaisesRegex(SystemExit, "references missing local asset"):
            self.verify_assets(files)

    def test_does_not_treat_external_urls_as_local_dependencies(self):
        files = self.application()
        files["assets/main.js"] = 'const url = "https://example.com/library.js";'
        self.assertEqual(self.verify_assets(files), ["assets/main.css", "assets/main.js"])

    def test_ignores_extension_checks_emitted_by_vite_preload_helper(self):
        files = self.application()
        files["assets/main.js"] = 'const isStylesheet = asset => asset.endsWith(".css");'
        self.assertEqual(self.verify_assets(files), ["assets/main.css", "assets/main.js"])


if __name__ == "__main__":
    unittest.main()
