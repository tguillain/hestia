"""Read-only inventory of a Home Assistant instance.

Usage: python ha-inventory.py <path/to/.env> <output-dir>
The .env file must define HA_URL and HA_TOKEN (long-lived access token).
"""

import asyncio
import json
import ssl
import sys
import urllib.parse
import urllib.request
from pathlib import Path

import websockets

WS_COMMANDS = {
    "config": {"type": "get_config"},
    "states": {"type": "get_states"},
    "areas": {"type": "config/area_registry/list"},
    "floors": {"type": "config/floor_registry/list"},
    "labels": {"type": "config/label_registry/list"},
    "devices": {"type": "config/device_registry/list"},
    "entities": {"type": "config/entity_registry/list"},
    "config_entries": {"type": "config_entries/get"},
    "dashboards": {"type": "lovelace/dashboards/list"},
    "lovelace_default": {"type": "lovelace/config"},
    "cloud": {"type": "cloud/status"},
    "hardware": {"type": "hardware/info"},
    "addons": {"type": "supervisor/api", "endpoint": "/addons", "method": "get"},
    "host": {"type": "supervisor/api", "endpoint": "/host/info", "method": "get"},
    "os": {"type": "supervisor/api", "endpoint": "/os/info", "method": "get"},
}

REST_CONFIG_DOMAINS = {"automation": "id", "script": None, "scene": "id"}


def load_env(path: Path) -> dict[str, str]:
    env = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, value = line.split("=", 1)
            env[key.strip()] = value.strip()
    return env


def rest_get(base: str, token: str, path: str):
    request = urllib.request.Request(
        base + path, headers={"Authorization": f"Bearer {token}"}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except Exception as error:
        return {"error": str(error)}


async def run_ws(base: str, token: str) -> dict:
    parts = urllib.parse.urlsplit(base)
    scheme = "wss" if parts.scheme == "https" else "ws"
    url = f"{scheme}://{parts.netloc}/api/websocket"
    results = {}
    async with websockets.connect(
        url, max_size=None, ssl=ssl.create_default_context() if scheme == "wss" else None
    ) as ws:
        await ws.recv()
        await ws.send(json.dumps({"type": "auth", "access_token": token}))
        auth = json.loads(await ws.recv())
        if auth.get("type") != "auth_ok":
            raise SystemExit(f"Authentication failed: {auth}")
        results["ha_version"] = auth.get("ha_version")
        message_id = 0

        async def call(command: dict):
            nonlocal message_id
            message_id += 1
            await ws.send(json.dumps({"id": message_id, **command}))
            while True:
                reply = json.loads(await ws.recv())
                if reply.get("id") == message_id and reply.get("type") == "result":
                    return reply["result"] if reply.get("success") else {"error": reply.get("error")}

        for name, command in WS_COMMANDS.items():
            results[name] = await call(command)
        if isinstance(results["dashboards"], list):
            for dashboard in results["dashboards"]:
                url_path = dashboard["url_path"]
                results[f"lovelace_{url_path}"] = await call(
                    {"type": "lovelace/config", "url_path": url_path}
                )
    return results


def fetch_configs(base: str, token: str, states: list) -> dict:
    configs = {}
    for domain, id_attribute in REST_CONFIG_DOMAINS.items():
        configs[domain] = {}
        for state in states:
            entity_id = state["entity_id"]
            if not entity_id.startswith(domain + "."):
                continue
            key = state["attributes"].get(id_attribute) if id_attribute else entity_id.split(".", 1)[1]
            if key is None:
                configs[domain][entity_id] = {"error": "no editable id (YAML-only?)"}
                continue
            configs[domain][entity_id] = rest_get(base, token, f"/api/config/{domain}/config/{key}")
    return configs


def main() -> None:
    env = load_env(Path(sys.argv[1]))
    output = Path(sys.argv[2])
    output.mkdir(parents=True, exist_ok=True)
    base = env["HA_URL"].rstrip("/")
    token = env["HA_TOKEN"]

    results = asyncio.run(run_ws(base, token))
    states = results["states"] if isinstance(results["states"], list) else []
    results.update(fetch_configs(base, token, states))

    for name, data in results.items():
        (output / f"{name}.json").write_text(
            json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8"
        )
        size = len(data) if isinstance(data, (list, dict)) else 1
        print(f"{name}: {size}")


if __name__ == "__main__":
    main()
