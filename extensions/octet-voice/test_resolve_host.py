"""Host-source resolution is fail-closed and independent of network in unit tests."""
import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("resolve_host", Path(__file__).parent / "scripts/resolve_host.py")
resolve_host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(resolve_host)


SHA = "0123456789abcdef0123456789abcdef01234567"
PULL = {
    "state": "open",
    "head": {"repo": {"full_name": resolve_host.REPOSITORY}, "sha": SHA},
    "base": {"repo": {"full_name": resolve_host.REPOSITORY}, "ref": "main"},
}
RELEASE = {"tag_name": "v0.10.0", "draft": False, "prerelease": False}


class HostResolutionTests(unittest.TestCase):
    def test_candidate_is_exact_open_pr_head(self):
        paths = []
        def get(path):
            paths.append(path)
            return PULL
        self.assertEqual(resolve_host.resolve("candidate", get), SHA)
        self.assertEqual(paths, ["pulls/639"])

    def test_auto_uses_open_pr_or_tag_after_pr_closes(self):
        self.assertEqual(resolve_host.resolve("auto", lambda _: PULL), SHA)
        closed = copy.deepcopy(PULL)
        closed["state"] = "closed"
        def get(path):
            return closed if path == "pulls/639" else {"object": {"type": "commit", "sha": SHA}}
        self.assertEqual(resolve_host.resolve("auto", get), SHA)
        with self.assertRaises(ValueError):
            resolve_host.resolve("auto", lambda _: {})

    def test_rejects_closed_wrong_repository_and_wrong_base(self):
        for edit in (
            lambda p: p.update(state="closed"),
            lambda p: p["head"]["repo"].update(full_name="someone/fork"),
            lambda p: p["base"]["repo"].update(full_name="someone/fork"),
            lambda p: p["base"].update(ref="other"),
            lambda p: p["head"].update(repo=None),
        ):
            pull = copy.deepcopy(PULL)
            edit(pull)
            with self.subTest(pull=pull), self.assertRaises(ValueError):
                resolve_host.resolve("candidate", lambda _: pull)

    def test_release_requires_published_stable_release_and_tag_commit(self):
        paths = []
        def get(path):
            paths.append(path)
            return RELEASE if path.startswith("releases/") else {"object": {"type": "commit", "sha": SHA}}
        self.assertEqual(resolve_host.resolve("release", get), SHA)
        self.assertEqual(paths, ["releases/tags/v0.10.0", "git/ref/tags/v0.10.0"])

    def test_tag_qualification_resolves_annotated_tag_without_a_release(self):
        tag = "a" * 40
        paths = []
        def get(path):
            paths.append(path)
            if path == "git/ref/tags/v0.10.0":
                return {"object": {"type": "tag", "sha": tag}}
            return {"object": {"type": "commit", "sha": SHA}}
        self.assertEqual(resolve_host.resolve("tag", get), SHA)
        self.assertEqual(paths, ["git/ref/tags/v0.10.0", f"git/tags/{tag}"])

    def test_invalid_or_cyclic_tag_fails_closed(self):
        for obj in ({"type": "blob", "sha": SHA}, {"type": "tag", "sha": SHA},
                    {"type": "commit", "sha": "main"}, {}):
            with self.subTest(obj=obj), self.assertRaises(ValueError):
                resolve_host.resolve("tag", lambda _: {"object": obj})

    def test_rejects_draft_prerelease_wrong_or_missing_release_fields(self):
        for edit in (
            lambda r: r.update(draft=True),
            lambda r: r.update(prerelease=True),
            lambda r: r.update(tag_name="v0.9.0"),
            lambda r: r.pop("draft"),
        ):
            release = RELEASE.copy()
            edit(release)
            with self.subTest(release=release), self.assertRaises(ValueError):
                resolve_host.resolve("release", lambda _: release)

    def test_rejects_nonimmutable_sha_and_invalid_target(self):
        for value in ("main", SHA[:12], SHA.upper(), "x" * 40, None):
            pull = copy.deepcopy(PULL)
            pull["head"]["sha"] = value
            with self.subTest(sha=value), self.assertRaises(ValueError):
                resolve_host.resolve("candidate", lambda _: pull)
        with self.assertRaises(ValueError):
            resolve_host.resolve("other", lambda _: PULL)

    def test_api_failure_does_not_accept_output(self):
        with patch.object(resolve_host.subprocess, "run") as run:
            run.return_value.returncode = 1
            run.return_value.stdout = '{"sha": "' + SHA + '"}'
            with self.assertRaises(RuntimeError):
                resolve_host.github_get("pulls/639")


if __name__ == "__main__":
    unittest.main()
