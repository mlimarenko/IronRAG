"""HTTPS clients whose peer certificate must match one trusted SHA-256."""

from __future__ import annotations

import hashlib
import http.client
import select
import socket
import ssl
import threading
import urllib.request
from typing import Any

import requests
from requests.adapters import HTTPAdapter


def pinned_tls_context(*, cadata: str | None = None, cafile: str | None = None) -> ssl.SSLContext:
    """Build the one TLS context this tree uses, with its guarantees written down.

    `ssl.create_default_context` already verifies the chain and the hostname and
    already refuses anything below TLS 1.2 on a supported interpreter. Stating
    all three here means the guarantee is read from this file rather than
    inferred from whichever interpreter happens to run the benchmark.
    """
    context = ssl.create_default_context(cadata=cadata, cafile=cafile)
    context.check_hostname = True
    context.verify_mode = ssl.CERT_REQUIRED
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    return context


def verify_peer_der(peer_der: bytes | None, expected_sha256: str) -> None:
    expected = validated_certificate_sha256(expected_sha256)
    if not peer_der or hashlib.sha256(peer_der).hexdigest() != expected:
        raise ssl.SSLError("dependency TLS peer differs from trusted SHA-256")


class PinnedDependencyProxy:
    """Process-owned loopback proxy enforcing exact DER on every upstream TLS session."""

    def __init__(
        self, host: str, port: int, expected_sha256: str, protocol: str, pinned_der: bytes
    ) -> None:
        self.host = host
        self.port = port
        self.expected_sha256 = validated_certificate_sha256(expected_sha256)
        if hashlib.sha256(pinned_der).hexdigest() != self.expected_sha256:
            raise ValueError("dependency TLS certificate bytes differ from trusted SHA-256")
        self.pinned_pem = ssl.DER_cert_to_PEM_cert(pinned_der)
        if protocol not in {"postgres", "redis"}:
            raise ValueError("unsupported pinned dependency protocol")
        self.protocol = protocol
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.local_port = int(self.listener.getsockname()[1])
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self) -> None:
        while True:
            try:
                downstream, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(downstream,), daemon=True).start()

    def _serve(self, downstream: socket.socket) -> None:
        try:
            with downstream, socket.create_connection((self.host, self.port), timeout=15) as raw:
                if self.protocol == "postgres":
                    raw.sendall(b"\x00\x00\x00\x08\x04\xd2\x16\x2f")
                    if raw.recv(1) != b"S":
                        raise ssl.SSLError("PostgreSQL dependency refused TLS")
                context = pinned_tls_context(cadata=self.pinned_pem)
                with context.wrap_socket(raw, server_hostname=self.host) as upstream:
                    peer = upstream.getpeercert(binary_form=True)
                    verify_peer_der(peer, self.expected_sha256)
                    self._forward(downstream, upstream)
        except OSError:
            return

    @staticmethod
    def _forward(left: socket.socket, right: socket.socket) -> None:
        sockets = [left, right]
        while sockets:
            readable, _, _ = select.select(sockets, [], [], 30)
            if not readable:
                continue
            for source in readable:
                data = source.recv(64 * 1024)
                if not data:
                    return
                (right if source is left else left).sendall(data)


_DEPENDENCY_PROXIES: list[PinnedDependencyProxy] = []


def activate_pinned_dependency_proxy(
    host: str, port: int, expected_sha256: str, protocol: str, pinned_der: bytes
) -> int:
    proxy = PinnedDependencyProxy(host, port, expected_sha256, protocol, pinned_der)
    _DEPENDENCY_PROXIES.append(proxy)
    return proxy.local_port


def validated_certificate_sha256(value: str) -> str:
    if (
        len(value) != 64
        or value == "0" * 64
        or any(character not in "0123456789abcdef" for character in value)
    ):
        raise ValueError("TLS certificate pin must be a non-zero lowercase SHA-256")
    return value


class PinnedHTTPSConnection(http.client.HTTPSConnection):
    def __init__(self, host: str, *, expected_sha256: str, **kwargs: Any) -> None:
        self.expected_sha256 = validated_certificate_sha256(expected_sha256)
        super().__init__(host, **kwargs)

    def connect(self) -> None:
        super().connect()
        certificate = self.sock.getpeercert(binary_form=True) if self.sock is not None else None
        observed = hashlib.sha256(certificate or b"").hexdigest()
        if not certificate or observed != self.expected_sha256:
            self.close()
            raise ssl.SSLError("HTTPS peer certificate differs from trusted SHA-256")


class PinnedHTTPSHandler(urllib.request.HTTPSHandler):
    def __init__(self, expected_sha256: str) -> None:
        super().__init__(context=pinned_tls_context())
        self.expected_sha256 = validated_certificate_sha256(expected_sha256)

    def https_open(self, request: urllib.request.Request):
        def connection(host: str, **kwargs: Any) -> PinnedHTTPSConnection:
            return PinnedHTTPSConnection(
                host,
                expected_sha256=self.expected_sha256,
                **kwargs,
            )

        return self.do_open(connection, request, context=self._context)


class FingerprintAdapter(HTTPAdapter):
    def __init__(self, expected_sha256: str) -> None:
        self.expected_sha256 = validated_certificate_sha256(expected_sha256)
        super().__init__()

    def init_poolmanager(self, connections: int, maxsize: int, block: bool = False, **kwargs: Any) -> None:
        kwargs["assert_fingerprint"] = self.expected_sha256
        super().init_poolmanager(connections, maxsize, block=block, **kwargs)

    def proxy_manager_for(self, proxy: str, **proxy_kwargs: Any):
        proxy_kwargs["assert_fingerprint"] = self.expected_sha256
        return super().proxy_manager_for(proxy, **proxy_kwargs)


def pinned_urllib_opener(expected_sha256: str, *handlers: Any) -> urllib.request.OpenerDirector:
    return urllib.request.build_opener(PinnedHTTPSHandler(expected_sha256), *handlers)


def pinned_requests_session(expected_sha256: str) -> requests.Session:
    session = requests.Session()
    session.mount("https://", FingerprintAdapter(expected_sha256))
    return session
