import importlib.util
import json
import plistlib
import stat
import tempfile
import unittest
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
MANAGER_PATH = REPO / "scripts/release/release_manager.py"
SPEC = importlib.util.spec_from_file_location("release_manager", MANAGER_PATH)
release_manager = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(release_manager)


class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.stable = self.root / "Applications/Kokoro Voice 2.1.app"
        self.artifact = self.root / "artifacts/Kokoro Voice 2.1.app"
        self.candidate = self.root / "artifacts/Kokoro Voice Candidate.app"
        self.vault = self.root / "vault"
        self.config_dir = self.root / "config"
        self.codesign = self.root / "fake-codesign"
        self.codesign.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"-dv\" ]; then\n"
            "  echo 'TeamIdentifier=not set' >&2\n"
            "fi\n"
            "exit 0\n"
        )
        self.codesign.chmod(self.codesign.stat().st_mode | stat.S_IXUSR)
        self.runtime = {
            "stable_app": self.stable,
            "vault": self.vault,
            "config": self.config_dir,
            "codesign": str(self.codesign),
            "ditto": "/usr/bin/false",
            "open": "/usr/bin/false",
            "osascript": "/usr/bin/false",
            "copy_mode": "python",
            "skip_launch": True,
            "skip_health": True,
            "skip_accessibility": False,
            "accessibility_timeout": 0.1,
            "health_url": "http://127.0.0.1:1/health",
        }
        self.make_bundle(
            self.stable, release_manager.STABLE_IDENTIFIER, "2.1.0-beta.1"
        )
        self.make_bundle(
            self.artifact, release_manager.STABLE_IDENTIFIER, "2.1.1-beta.1"
        )
        self.make_bundle(
            self.candidate, release_manager.CANDIDATE_IDENTIFIER, "2.1.1-beta.1"
        )

    def tearDown(self):
        self.temporary.cleanup()

    @staticmethod
    def make_bundle(path: Path, identifier: str, version: str) -> None:
        executable = path / "Contents/MacOS/kokoro"
        executable.parent.mkdir(parents=True)
        executable.write_text("binary")
        with (path / "Contents/Info.plist").open("wb") as handle:
            plistlib.dump(
                {
                    "CFBundleIdentifier": identifier,
                    "CFBundleShortVersionString": version,
                    "CFBundleExecutable": "kokoro",
                },
                handle,
            )

    def installed_version(self) -> str:
        return release_manager.bundle_info(self.stable)["CFBundleShortVersionString"]

    def test_candidate_and_stable_artifacts_have_distinct_required_identities(self):
        candidate = release_manager.verify_bundle(
            self.candidate, release_manager.CANDIDATE_IDENTIFIER, self.runtime
        )
        stable = release_manager.verify_bundle(
            self.artifact, release_manager.STABLE_IDENTIFIER, self.runtime
        )
        self.assertNotEqual(candidate["identifier"], stable["identifier"])
        with self.assertRaises(release_manager.ReleaseError):
            release_manager.verify_bundle(
                self.candidate, release_manager.STABLE_IDENTIFIER, self.runtime
            )

    def test_release_health_requires_matching_version_and_authentication(self):
        valid = {
            "status": "ok",
            "service_version": "2.1.1-beta.1",
            "auth_required": True,
            "stt_cache_mode": "owned",
        }
        self.assertTrue(release_manager.acceptable_health(valid, "2.1.1-beta.1"))
        self.assertFalse(
            release_manager.acceptable_health(
                {**valid, "auth_required": False}, "2.1.1-beta.1"
            )
        )
        self.assertFalse(release_manager.acceptable_health(valid, "2.1.0-beta.1"))

    def test_ad_hoc_update_requires_an_explicit_override(self):
        with self.assertRaisesRegex(release_manager.ReleaseError, "ad-hoc signed"):
            release_manager.promote(
                self.artifact, allow_ad_hoc=False, config=self.runtime
            )
        self.assertEqual(self.installed_version(), "2.1.0-beta.1")

    def test_first_developer_id_release_requires_transition_acknowledgement(self):
        transition_codesign = self.root / "transition-codesign"
        transition_codesign.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"-dv\" ]; then\n"
            "  case \"$*\" in\n"
            "    *artifacts*) echo 'TeamIdentifier=TEAM123' >&2 ;;\n"
            "    *) echo 'TeamIdentifier=not set' >&2 ;;\n"
            "  esac\n"
            "fi\n"
            "exit 0\n"
        )
        transition_codesign.chmod(
            transition_codesign.stat().st_mode | stat.S_IXUSR
        )
        runtime = {**self.runtime, "codesign": str(transition_codesign)}

        with self.assertRaisesRegex(release_manager.ReleaseError, "reset macOS permissions"):
            release_manager.promote(
                self.artifact,
                allow_ad_hoc=False,
                allow_signing_transition=False,
                config=runtime,
            )
        self.assertEqual(self.installed_version(), "2.1.0-beta.1")

    def test_signed_updates_require_the_same_designated_requirement(self):
        base = {
            "identifier": release_manager.STABLE_IDENTIFIER,
            "team_id": "TEAM123",
            "designated_requirement": "identifier stable and team TEAM123",
        }
        release_manager.require_compatible_update_identity(base, dict(base))
        with self.assertRaisesRegex(
            release_manager.ReleaseError, "designated requirement"
        ):
            release_manager.require_compatible_update_identity(
                base,
                {
                    **base,
                    "designated_requirement": "identifier stable and team OTHER",
                },
            )

    def test_promotion_and_rollback_are_reversible(self):
        self.config_dir.mkdir()
        (self.config_dir / "prefs.json").write_text('{"voice":"old"}\n')
        (self.config_dir / "token").write_text("must-not-be-archived")

        promoted = release_manager.promote(
            self.artifact, allow_ad_hoc=True, config=self.runtime
        )
        self.assertEqual(self.installed_version(), "2.1.1-beta.1")
        rollback_config = Path(promoted["rollback"]["config"])
        self.assertTrue((rollback_config / "prefs.json").is_file())
        self.assertFalse((rollback_config / "token").exists())

        (self.config_dir / "prefs.json").write_text('{"voice":"new"}\n')
        rolled_back = release_manager.rollback(self.runtime)
        self.assertEqual(self.installed_version(), "2.1.0-beta.1")
        self.assertEqual(
            (self.config_dir / "prefs.json").read_text(), '{"voice":"old"}\n'
        )
        self.assertEqual(
            rolled_back["rollback"]["metadata"]["version"], "2.1.1-beta.1"
        )

    def test_failed_candidate_health_restores_the_previous_stable(self):
        previous_check = release_manager.launch_and_check
        calls = []

        def fail_once(*_args, **_kwargs):
            calls.append("check")
            if len(calls) == 1:
                raise release_manager.ReleaseError("simulated unhealthy engine")

        release_manager.launch_and_check = fail_once
        try:
            with self.assertRaisesRegex(
                release_manager.ReleaseError, "Stable was restored"
            ):
                release_manager.promote(
                    self.artifact, allow_ad_hoc=True, config=self.runtime
                )
        finally:
            release_manager.launch_and_check = previous_check

        self.assertEqual(calls, ["check", "check"])
        self.assertEqual(self.installed_version(), "2.1.0-beta.1")

    def test_same_version_update_is_rejected_before_the_swap(self):
        same = self.root / "same/Kokoro Voice 2.1.app"
        self.make_bundle(same, release_manager.STABLE_IDENTIFIER, "2.1.0-beta.1")
        with self.assertRaisesRegex(release_manager.ReleaseError, "must differ"):
            release_manager.promote(same, allow_ad_hoc=True, config=self.runtime)
        self.assertEqual(self.installed_version(), "2.1.0-beta.1")

    def test_readiness_evidence_must_match_version_process_and_cutover(self):
        event_file = self.config_dir / "events.jsonl"
        event_file.parent.mkdir(parents=True)
        valid = {
            "timestamp_ms": 2000,
            "event": "runtime-readiness",
            "app_version": "2.1.1-beta.1",
            "process_id": 123,
            "fields": {
                "ready": True,
                "accessibility": True,
                "input_monitoring": True,
                "hotkeys_registered": True,
            },
        }
        records = [
            {**valid, "timestamp_ms": 999},
            {**valid, "app_version": "2.1.0-beta.1"},
            {**valid, "process_id": 999},
            valid,
        ]
        event_file.write_text(
            "not-json\n"
            + json.dumps({**valid, "timestamp_ms": "corrupt"})
            + "\n"
            + "\n".join(json.dumps(record) for record in records)
        )

        result = release_manager.latest_runtime_readiness(
            event_file,
            expected_version="2.1.1-beta.1",
            process_ids={123},
            not_before_ms=1000,
        )
        self.assertTrue(result["ready"])
        self.assertIsNone(
            release_manager.latest_runtime_readiness(
                event_file,
                expected_version="2.1.1-beta.1",
                process_ids={456},
                not_before_ms=1000,
            )
        )

    def test_readiness_wait_follows_a_macos_permission_restart(self):
        event_file = self.config_dir / "events.jsonl"
        event_file.parent.mkdir(parents=True)
        event_file.write_text(
            json.dumps(
                {
                    "timestamp_ms": 2000,
                    "event": "runtime-readiness",
                    "app_version": "2.1.1-beta.1",
                    "process_id": 456,
                    "fields": {
                        "ready": True,
                        "accessibility": True,
                        "input_monitoring": True,
                        "hotkeys_registered": True,
                    },
                }
            )
            + "\n"
        )
        previous_app_pids = release_manager.app_pids
        release_manager.app_pids = lambda _app: [456]
        try:
            release_manager.wait_for_runtime_readiness(
                self.runtime,
                app=self.stable,
                expected_version="2.1.1-beta.1",
                process_ids={123},
                not_before_ms=1000,
            )
        finally:
            release_manager.app_pids = previous_app_pids


if __name__ == "__main__":
    unittest.main()
