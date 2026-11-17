# API clients and upstream integration

[中文](../protocols.md) · [English index](README.md)

DivertLink forwards HTTP semantics and streams response chunks without parsing model JSON. It supports HTTP/1.1, HTTP/2 ingress and SSE; WebSocket, gRPC and HTTP/3 forwarding are unsupported. It does not translate one model API into another or rename models.

## Base URLs

| Client | Base URL for mount `/home` | Resulting request |
| --- | --- | --- |
| OpenAI SDK | `https://gateway.example.com/home/v1` | `/home/v1/chat/completions` |
| Anthropic SDK | `https://gateway.example.com/home` | `/home/v1/messages` |
| One agent with `/qwen` target prefix | `https://gateway.example.com/mixed/qwen/v1` | `/mixed/qwen/v1/chat/completions` |

The gateway removes the tunnel mount, and the agent removes any selected target prefix. The remaining path reaches the upstream unchanged. A host-only route may omit a mount, but requires matching DNS/Host and certificate.

Set `DIVERTLINK_API_KEY` privately to a **gateway API key**, then use the SDK matching the upstream:

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="https://gateway.example.com/home/v1",
    api_key=os.environ["DIVERTLINK_API_KEY"],
)
print(client.models.list())
```

For a private CA configure an HTTP client rather than disabling TLS:

```python
import httpx
client = OpenAI(
    base_url="https://gateway.example.com/home/v1",
    api_key=os.environ["DIVERTLINK_API_KEY"],
    http_client=httpx.Client(verify="/private/path/ca.crt"),
)
```

Anthropic example (the upstream must implement its API):

```python
import os
import anthropic

client = anthropic.Anthropic(
    base_url="https://gateway.example.com/home",
    api_key=os.environ["DIVERTLINK_API_KEY"],
)
# An actual generation consumes upstream model resources.
response = client.messages.create(
    model="YOUR_MODEL", max_tokens=32,
    messages=[{"role": "user", "content": "Hello"}],
)
print(response.content)
```

Configure gateway `upstream_auth_header = "x-api-key"` with the upstream key when the Anthropic-compatible service requires a separate key. Public Bearer and `x-api-key` authentication are both supported by the gateway. See [security](security.md) for the difference between client and upstream authentication.

## SSE

Use `stream=True` in the OpenAI SDK or `curl -N` with `"stream": true` in the JSON request. Verify chunks arrive before generation completes. The tunnel preserves streaming and cancellation; a front proxy can still buffer. Disable nginx buffering/compression or use Caddy `flush_interval -1`; check gateway stream-idle and upstream request timeouts.

## new-api / one-api

1. Use the gateway as a new-api channel. For an OpenAI channel the base is usually `https://gateway.example.com/home` because the channel appends `/v1`; an OpenAI-compatible channel may expect `/home/v1`. Confirm the channel's actual test URL. The channel key is a gateway API key, with gateway replacement authentication configured for the model's real upstream key.
2. Proxy a gateway `[[static_routes]]` mount `/newapi` to private `http://127.0.0.1:3000`. Authentication defaults to gateway keys. If `require_auth = false`, new-api must validate its own user keys; do not publicly expose its upstream port.
3. Point the home agent at new-api on its private local address. new-api handles model-name routing, quotas and provider transforms; DivertLink handles the encrypted transport.

Client model IDs must match the model service, unless new-api or another upstream layer explicitly maps them. Neither tunnel `protocols` metadata nor a URL prefix rewrites model IDs.
