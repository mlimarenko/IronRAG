import hashlib
import ssl
import unittest
from unittest import mock

from apps.api.benchmarks.benchmark_test_support import load_benchmark_module


MODULE = load_benchmark_module("tls_pinning_tests", "apps/api/benchmarks/tls_pinning.py")


class FakeSocket:
    def __init__(self, certificate: bytes) -> None:
        self.certificate = certificate

    def getpeercert(self, *, binary_form: bool = False):
        return self.certificate if binary_form else {}

    def close(self) -> None:
        pass


class TlsPinningTests(unittest.TestCase):
    def test_connection_rejects_peer_certificate_swap(self) -> None:
        trusted_certificate = b"trusted-certificate"
        trusted_sha256 = hashlib.sha256(trusted_certificate).hexdigest()

        def connect_with(certificate: bytes):
            def fake_connect(connection) -> None:
                connection.sock = FakeSocket(certificate)
            return fake_connect

        accepted = MODULE.PinnedHTTPSConnection(
            "api.invalid", expected_sha256=trusted_sha256
        )
        with mock.patch.object(
            MODULE.http.client.HTTPSConnection,
            "connect",
            connect_with(trusted_certificate),
        ):
            accepted.connect()

        swapped = MODULE.PinnedHTTPSConnection(
            "api.invalid", expected_sha256=trusted_sha256
        )
        with mock.patch.object(
            MODULE.http.client.HTTPSConnection,
            "connect",
            connect_with(b"another-valid-certificate"),
        ):
            with self.assertRaisesRegex(ssl.SSLError, "differs"):
                swapped.connect()

    def test_zero_or_malformed_pin_fails_closed(self) -> None:
        for invalid in ("", "0" * 64, "A" * 64):
            with self.subTest(invalid=invalid):
                with self.assertRaises(ValueError):
                    MODULE.validated_certificate_sha256(invalid)

    def test_requests_session_applies_pin_to_connection_pool(self) -> None:
        trusted = "1" * 64
        session = MODULE.pinned_requests_session(trusted)
        adapter = session.get_adapter("https://api.invalid")
        self.assertEqual(adapter.poolmanager.connection_pool_kw["assert_fingerprint"], trusted)

    def test_dependency_proxy_accepts_only_exact_peer_der(self) -> None:
        pinned = b"exact dependency leaf"
        expected = hashlib.sha256(pinned).hexdigest()

        MODULE.verify_peer_der(pinned, expected)
        with self.assertRaisesRegex(ssl.SSLError, "differs"):
            MODULE.verify_peer_der(b"another CA-valid leaf", expected)


if __name__ == "__main__":
    unittest.main()
