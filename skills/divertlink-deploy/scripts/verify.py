#!/usr/bin/env python3
"""Check local readiness and optional OpenAI-compatible ingress without exposing keys."""
import argparse
import ipaddress
import json
import os
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request


class VerifyError(ValueError):
    """A sanitized error suitable for public output."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def validate_url(url, admin=False):
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
        raise VerifyError("Use an HTTP(S) URL without embedded credentials, queries or fragments")
    try:
        loopback = parts.hostname.lower() == "localhost" or ipaddress.ip_address(parts.hostname).is_loopback
    except ValueError:
        loopback = parts.hostname.lower() == "localhost"
    if admin and (not loopback or parts.path not in ("", "/")):
        raise VerifyError("Admin checks require a loopback root URL; run on the gateway or use an SSH forward")
    if parts.scheme == "http" and not loopback:
        raise VerifyError("Remote ingress verification requires HTTPS")


def get(url, context, key=None):
    headers = {"Accept": "application/json"}
    if key:
        headers["Authorization"] = "Bearer " + key
    handlers = [NoRedirect(), urllib.request.HTTPSHandler(context=context)]
    # Local admin requests must not be routed through environment proxies.
    if urllib.parse.urlsplit(url).hostname in ("localhost", "127.0.0.1", "::1"):
        handlers.append(urllib.request.ProxyHandler({}))
    opener = urllib.request.build_opener(*handlers)
    try:
        with opener.open(urllib.request.Request(url, headers=headers), timeout=15) as response:
            return response.status, response.read(1024 * 1024)
    except urllib.error.HTTPError as error:
        return error.code, b""
    except urllib.error.URLError as error:
        if (not ssl.HAS_TLSv1_3 and isinstance(error.reason, ssl.SSLError)
                and "PROTOCOL_VERSION" in str(error.reason).upper()):
            raise VerifyError("This Python SSL backend lacks TLS 1.3; use a modern Python/OpenSSL build for the default HTTPS ingress") from None
        raise


def verify(admin_url, ingress_url=None, key_env="DIVERTLINK_API_KEY", ca=None, tunnel_id=None):
    validate_url(admin_url, admin=True)
    if ingress_url:
        validate_url(ingress_url)
    context = ssl.create_default_context(cafile=ca)
    checks = []
    for endpoint in ("healthz", "readyz"):
        status, body = get(admin_url.rstrip("/") + "/" + endpoint, context)
        ok = status == 200
        if endpoint == "readyz" and ok:
            ok = json.loads(body).get("ready") is True
        checks.append({"check": endpoint, "status": status, "ok": ok})
    if tunnel_id:
        status, body = get(admin_url.rstrip("/") + "/status", context)
        tunnels = json.loads(body).get("tunnels", []) if status == 200 else []
        connected = any(t.get("id") == tunnel_id and t.get("connected") is True for t in tunnels)
        checks.append({"check": "selected_tunnel_connected", "status": status, "ok": connected})
    if ingress_url:
        key = os.environ.get(key_env)
        if not key:
            raise VerifyError("Set the API key environment variable before checking ingress")
        models = ingress_url.rstrip("/") + "/models"
        status, _ = get(models, context)
        checks.append({"check": "unauthenticated_models_rejected", "status": status, "ok": status == 401})
        status, body = get(models, context, key)
        ok = status == 200
        if ok:
            payload = json.loads(body)
            ok = isinstance(payload, dict) and isinstance(payload.get("data"), list)
        checks.append({"check": "authenticated_models", "status": status, "ok": ok})
    return {"ok": all(item["ok"] for item in checks), "checks": checks}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--admin-url", default="http://127.0.0.1:9090")
    parser.add_argument("--ingress-url", help="OpenAI base URL including mount and /v1, e.g. https://gw.example/home/v1")
    parser.add_argument("--key-env", default="DIVERTLINK_API_KEY", help="Name of existing env var; never pass a key as an argument")
    parser.add_argument("--ca", help="CA file for a self-signed ingress; never disables TLS verification")
    parser.add_argument("--tunnel-id", help="Check this tunnel, not just any connected agent")
    args = parser.parse_args()
    try:
        report = verify(args.admin_url, args.ingress_url, args.key_env, args.ca, args.tunnel_id)
    except Exception as error:
        message = str(error) if isinstance(error, VerifyError) else type(error).__name__
        report = {"ok": False, "error": message}
    print(json.dumps(report))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
