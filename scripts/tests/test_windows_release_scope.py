from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "verify-windows-release.py"
    spec = importlib.util.spec_from_file_location("verify_windows_release", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleaseScopeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.verify = load_script()

    def test_current_readme_is_windows_only(self) -> None:
        scope = self.verify.verify_release_scope()
        self.assertEqual(scope["target"], "windows-10/11-x64")
        self.assertFalse(scope["macos_release"])
        self.assertFalse(scope["mobile_release"])

    def test_windows_release_uses_the_pinned_bun_version(self) -> None:
        self.assertEqual(self.verify.expected_bun_version(), "1.3.14")
        completed = self.verify.subprocess.CompletedProcess(
            ["bun", "--version"], 0, stdout="1.3.14\n", stderr=""
        )
        with patch.object(self.verify.subprocess, "run", return_value=completed):
            self.assertEqual(self.verify.verify_bun_version(), "1.3.14")

    def test_windows_release_rejects_a_different_bun_version(self) -> None:
        completed = self.verify.subprocess.CompletedProcess(
            ["bun", "--version"], 0, stdout="1.4.0\n", stderr=""
        )
        with patch.object(self.verify.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(SystemExit, "Bun 1.3.14 is required"):
                self.verify.verify_bun_version()

    def test_rejects_old_cross_platform_release_claims(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            readme = Path(directory) / "README.md"
            readme.write_text(
                "Windows 10/11 x64\n"
                "当前正式发布目标是 Windows 10/11 x64\n"
                "跨平台研究代码\n"
                "## macOS 构建与使用\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "non-Windows"):
                self.verify.verify_release_scope(readme)

    def test_production_rust_has_no_unbounded_channels(self) -> None:
        result = self.verify.verify_production_backpressure(
            (ROOT / "apps" / "windows" / "src", ROOT / "apps" / "windows-ui" / "src-tauri" / "src")
        )
        self.assertEqual(result["unbounded_channels"], 0)

    def test_rejects_an_unbounded_production_channel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "runtime.rs"
            source.write_text(
                "let (_sender, _receiver) = std::sync::mpsc::channel();\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.verify.verify_production_backpressure((Path(directory),))

    def test_rejects_an_aliased_unbounded_mpsc_channel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "tray.rs"
            source.write_text(
                "use std::sync::mpsc;\nlet (_sender, _receiver) = mpsc::channel();\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.verify.verify_production_backpressure((Path(directory),))

    def scan_rust_fixture(self, sources: dict[str, str]) -> dict[str, object]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, text in sources.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            return self.verify.verify_production_backpressure((root,))

    def test_ignores_only_the_exact_inline_test_item_with_nested_literals(self) -> None:
        self.scan_rust_fixture({"lib.rs": r'''
            #[cfg /* comment */ (test)]
            #[allow(dead_code)]
            mod checks {
                fn nested() {
                    let raw = br###"} ]; #[cfg(test)] { unbounded_channel()"###;
                    let normal = "escaped quote \" and braces } {";
                    let character = '\u{7D}';
                    /* nested { /* comment } */ still inside */
                    if true { let _ = std::sync::mpsc::channel(); }
                }
            }
            fn production() { let _ = tokio::sync::mpsc::channel::<u8>(1); }
        '''})

    def test_a_test_item_never_hides_a_production_neighbor(self) -> None:
        for item in [
            '#[cfg(test)] mod checks { fn t() { let _ = r#"} unbounded_channel() {"#; } }',
            '#[cfg(test)] const fn helper() -> usize { 0 }',
            '#[cfg(test)] const CHECK: usize = { if true { 1 } else { 0 } };',
        ]:
            with self.subTest(item=item), self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.scan_rust_fixture({"lib.rs": item + '\nfn production() { let _ = std::sync::mpsc::channel(); }'})

    def test_external_cfg_test_modules_and_their_children_are_test_only(self) -> None:
        result = self.scan_rust_fixture({
            "lib.rs": 'mod runtime;\n#[path = r"fixtures/custom.rs"]\n#[cfg(test)] mod custom;',
            "runtime.rs": '#[cfg(test)] mod checks;\nfn production() { let _ = tokio::sync::mpsc::channel::<u8>(1); }',
            "runtime/checks.rs": 'mod child;\nfn t() { let _ = std::sync::mpsc::channel(); }',
            "runtime/checks/child.rs": 'fn child() { let _ = tokio::sync::mpsc::unbounded_channel::<u8>(); }',
            "fixtures/custom.rs": 'fn arbitrary_filename() { let _ = mpsc::channel(); }',
        })
        self.assertEqual(result["checked_files"], 5)

    def test_shared_external_module_stays_scanned_when_also_production(self) -> None:
        with self.assertRaisesRegex(SystemExit, "shared.rs"):
            self.scan_rust_fixture({
                "lib.rs": '#[cfg(test)] #[path = "shared.rs"] mod checks;\n#[path = "shared.rs"] mod production;',
                "shared.rs": 'fn run() { let _ = std::sync::mpsc::channel(); }',
            })

    def test_test_filename_and_unknown_cfg_never_authorize_exclusion(self) -> None:
        for source in [
            'fn run() { let _ = mpsc::channel(); }',
            '#[cfg(any(test, feature = "testing"))] mod checks { fn t() { let _ = mpsc::channel(); } }',
            '#[cfg(not(test))] fn production() { let _ = mpsc::channel(); }',
        ]:
            with self.subTest(source=source), self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.scan_rust_fixture({"something_tests.rs": source})

    def test_comments_strings_and_characters_do_not_invent_cfg_items_or_channels(self) -> None:
        self.scan_rust_fixture({"lib.rs": r'''
            // #[cfg(test)] mod fake { std::sync::mpsc::channel();
            /* a nested comment /* } */ unbounded_channel() */
            const MESSAGE: &str = r##"#[cfg(test)] fn fake() { unbounded_channel(); }"##;
            fn production<'a>(text: &'a str) {
                let c = '}'; let quote = '\'';
                let s = "unbounded_channel() and escaped \" quote";
                let _ = tokio::sync::mpsc::channel::<u8>(1);
            }
        '''})
        with self.assertRaisesRegex(SystemExit, "unbounded channel"):
            self.scan_rust_fixture({"lib.rs": r'''
                const FAKE: &str = r#"#[cfg(test)] mod tests {"#;
                // #[cfg(test)] fn fake() {}
                fn production() { let _ = std::sync::mpsc::channel(); }
            '''})


if __name__ == "__main__":
    unittest.main()
