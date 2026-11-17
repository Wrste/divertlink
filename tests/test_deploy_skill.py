"""Deployment helper invariants. Optional real-binary tests: DIVERTLINK_TEST_BINARY=/path."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "skills" / "divertlink-deploy" / "scripts"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


installer = load("skill_install", SCRIPTS / "install_binary.py")
verifier = load("skill_verify", SCRIPTS / "verify.py")
packager = load("skill_package", ROOT / "scripts" / "package-skill.py")
NAME = "divertlink-0.1.0-x86_64-unknown-linux-musl"


def archive_bytes(kind="regular", duplicate=False):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for _ in range(2 if duplicate else 1):
            info = tarfile.TarInfo(NAME + "/divertlink")
            if kind == "symlink":
                info.type = tarfile.SYMTYPE
                info.linkname = "/tmp/outside"
                archive.addfile(info)
            else:
                content = b"#!/bin/sh\nprintf 'divertlink 0.1.0\\n'\n"
                info.size = len(content)
                archive.addfile(info, io.BytesIO(content))
        # Never extract unrelated paths, even if an archive contains one.
        outside = tarfile.TarInfo("../../outside")
        outside.size = 6
        archive.addfile(outside, io.BytesIO(b"secret"))
    return data.getvalue()


def zip_bytes(symlink=False, duplicate=False):
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w") as archive:
        info = zipfile.ZipInfo(NAME + "/divertlink.exe")
        if symlink:
            info.create_system = 3
            info.external_attr = 0o120777 << 16
            archive.writestr(info, b"/tmp/outside")
        else:
            archive.writestr(info, b"MZ")
        if duplicate:
            archive.writestr(NAME + "/divertlink.exe", b"second")
    return data.getvalue()


class InstallerTests(unittest.TestCase):
    def test_platforms_and_rejection(self):
        self.assertEqual(installer.target_for("Darwin", "arm64"), "aarch64-apple-darwin")
        self.assertEqual(installer.target_for("Windows", "AMD64"), "x86_64-pc-windows-msvc")
        for system, machine in (("Windows", "ARM64"), ("Linux", "i686"), ("Other", "x86_64")):
            with self.assertRaises(ValueError):
                installer.target_for(system, machine)

    def test_dry_run_has_no_download_or_writes(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(installer, "download", side_effect=AssertionError("network")):
            dest = Path(temp) / "absent"
            result = installer.install("v0.1.0", dest, dry_run=True)
            self.assertTrue(result["dry_run"])
            self.assertFalse(dest.exists())

    def test_checksum_failure_preserves_existing_binary(self):
        with tempfile.TemporaryDirectory() as temp, patch.object(installer.platform, "system", return_value="Linux"), patch.object(installer.platform, "machine", return_value="x86_64"):
            binary = Path(temp) / "divertlink"
            binary.write_bytes(b"previous")
            bad_checksum = ("0" * 64 + "  " + NAME + ".tar.gz").encode()
            with patch.object(installer, "download", side_effect=[bad_checksum, archive_bytes()]):
                with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                    installer.install("0.1.0", temp, replace=True)
            self.assertEqual(binary.read_bytes(), b"previous")
            with patch.object(installer, "download", side_effect=AssertionError("network")):
                with self.assertRaisesRegex(ValueError, "Binary exists"):
                    installer.install("0.1.0", temp)

    @unittest.skipIf(os.name == "nt", "fixture executable is a POSIX shell script")
    def test_verified_install_and_no_arbitrary_extraction(self):
        data = archive_bytes()
        checksum = (hashlib.sha256(data).hexdigest() + "  " + NAME + ".tar.gz").encode()
        with tempfile.TemporaryDirectory() as temp, patch.object(installer.platform, "system", return_value="Linux"), patch.object(installer.platform, "machine", return_value="x86_64"), patch.object(installer, "download", side_effect=[checksum, data]):
            destination = Path(temp) / "bin"
            report = installer.install("0.1.0", destination)
            self.assertTrue(report["installed"])
            self.assertEqual(sorted(p.name for p in destination.iterdir()), ["divertlink"])
            self.assertEqual(sorted(p.name for p in Path(temp).iterdir()), ["bin"])
            self.assertEqual(subprocess.check_output([str(destination / "divertlink"), "version"]).decode().strip(), "divertlink 0.1.0")

    def test_reject_symlink_and_duplicate_executables(self):
        for data in (archive_bytes("symlink"), archive_bytes(duplicate=True)):
            with self.assertRaises(ValueError):
                installer.unpack_binary(data, NAME, False)
        for data in (zip_bytes(symlink=True), zip_bytes(duplicate=True)):
            with self.assertRaises(ValueError):
                installer.unpack_binary(data, NAME, True)

    def test_reject_malformed_version_before_network(self):
        with patch.object(installer, "download", side_effect=AssertionError("network")):
            for version in ("latest", "../secret", "0.1.0?x", "0.1.0/evil"):
                with self.assertRaises(ValueError):
                    installer.install(version, "unused")


class Handler(BaseHTTPRequestHandler):
    requests_seen = []

    def log_message(self, *_):
        pass

    def do_GET(self):
        type(self).requests_seen.append((self.path, self.headers.get("Authorization")))
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/leak")
            self.end_headers()
            return
        if self.path == "/healthz":
            payload = {"ok": True}
        elif self.path == "/readyz":
            payload = {"ready": True}
        elif self.path == "/status":
            payload = {"tunnels": [{"id": "home", "connected": True}]}
        else:
            payload = {"object": "list", "data": [{"id": "mock"}]}
        body = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class VerificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.http = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.http.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = "http://127.0.0.1:" + str(cls.http.server_port)

    @classmethod
    def tearDownClass(cls):
        cls.http.shutdown()
        cls.http.server_close()
        cls.thread.join()

    def test_refuse_unsafe_urls(self):
        for url, admin in (("http://public.example", False), ("https://public.example", True), (self.base + "/path", True), ("https://user:secret@example.com", False), (self.base + "?q=x", False)):
            with self.assertRaises(ValueError):
                verifier.validate_url(url, admin)

    def test_selected_tunnel_not_any_tunnel(self):
        self.assertTrue(verifier.verify(self.base, tunnel_id="home")["ok"])
        self.assertFalse(verifier.verify(self.base, tunnel_id="missing")["ok"])

    def test_redirect_is_not_followed(self):
        Handler.requests_seen.clear()
        status, _ = verifier.get(self.base + "/redirect", ssl.create_default_context(), "secret")
        self.assertEqual(status, 302)
        self.assertEqual(Handler.requests_seen, [("/redirect", "Bearer secret")])

    def test_open_endpoint_does_not_pass_auth_check(self):
        with patch.dict(os.environ, {"DIVERTLINK_API_KEY": "secret"}):
            report = verifier.verify(self.base, self.base + "/v1")
        self.assertFalse(report["ok"])
        self.assertNotIn("secret", json.dumps(report))

    def test_invalid_key_error_does_not_leak_key(self):
        env = dict(os.environ, DIVERTLINK_API_KEY="private-secret\ninvalid")
        result = subprocess.run([os.sys.executable, str(SCRIPTS / "verify.py"), "--admin-url", self.base, "--ingress-url", self.base + "/v1"], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("private-secret", result.stdout + result.stderr)
        self.assertFalse(json.loads(result.stdout)["ok"])


class PackagingTests(unittest.TestCase):
    def test_portable_package_is_reproducible_and_scoped(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "skill.zip"
            packager.package(output)
            first = output.read_bytes()
            packager.package(output)
            self.assertEqual(output.read_bytes(), first)
            self.assertTrue(output.with_name("skill.zip.sha256").exists())
            with zipfile.ZipFile(output) as archive:
                self.assertIsNone(archive.testzip())
                files = archive.namelist()
                self.assertIn("divertlink-deploy/SKILL.md", files)
                self.assertIn("divertlink-deploy/LICENSE-MIT", files)
                self.assertIn("divertlink-deploy/scripts/verify.py", files)
                self.assertTrue(all(name.startswith("divertlink-deploy/") and "__pycache__" not in name and ".." not in name for name in files))


@unittest.skipUnless(os.environ.get("DIVERTLINK_TEST_BINARY"), "set DIVERTLINK_TEST_BINARY for real gateway/agent tests")
class RealTunnelTests(unittest.TestCase):
    def test_gateway_agent_and_tls_authentication(self):
        binary = str(Path(os.environ["DIVERTLINK_TEST_BINARY"]).resolve())
        processes = []
        upstream = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as temp:
                config = Path(temp)
                subprocess.run([binary, "init", "--role", "both", "--domain", "localhost", "--dir", temp], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                # Select unprivileged loopback ports; no existing config or service is changed.
                ports = []
                for _ in range(3):
                    with socket.socket() as listener:
                        listener.bind(("127.0.0.1", 0))
                        ports.append(listener.getsockname()[1])
                tls_port, plain_port, admin_port = ports
                server = config / "server.toml"
                text = server.read_text()
                key = re.search(r'^key = "(dlk_[^"]+)"', text, re.M).group(1)
                for field, port in (("tunnel_listen", tls_port), ("ingress_listen", tls_port), ("http_listen", plain_port), ("admin_listen", admin_port)):
                    text = re.sub(r"^" + field + r" = .*", field + ' = "127.0.0.1:' + str(port) + '"', text, flags=re.M)
                server.write_text(text)
                agent = config / "agent.toml"
                text = re.sub(r'^server = .*', 'server = "127.0.0.1:' + str(tls_port) + '"', agent.read_text(), flags=re.M)
                text = re.sub(r'^url = .*', 'url = "http://127.0.0.1:' + str(upstream.server_port) + '"', text, flags=re.M)
                agent.write_text(text)
                for role, path in (("server", server), ("agent", agent)):
                    processes.append(subprocess.Popen([binary, role, "-c", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
                admin = "http://127.0.0.1:" + str(admin_port)
                ingress = "https://127.0.0.1:" + str(tls_port) + "/home/v1"
                ca = str(config / "certs" / "ca.crt")
                deadline = time.monotonic() + 15
                while True:
                    try:
                        ready = verifier.verify(admin, tunnel_id="home")["ok"]
                        if ready:
                            break
                    except OSError:
                        pass
                    if time.monotonic() > deadline:
                        self.fail("Temporary gateway/agent did not become ready")
                    time.sleep(0.1)
                with patch.dict(os.environ, {"DIVERTLINK_API_KEY": key}):
                    report = verifier.verify(admin, ingress, ca=ca, tunnel_id="home")
                    self.assertTrue(report["ok"], report)
                    self.assertNotIn(key, json.dumps(report))
                    with self.assertRaises(verifier.urllib.error.URLError):
                        verifier.verify(admin, ingress, tunnel_id="home")  # Unknown private CA must fail.
                with patch.dict(os.environ, {"DIVERTLINK_API_KEY": "wrong"}):
                    self.assertFalse(verifier.verify(admin, ingress, ca=ca, tunnel_id="home")["ok"])
        finally:
            for process in processes:
                process.terminate()
            for process in processes:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            upstream.shutdown()
            upstream.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
