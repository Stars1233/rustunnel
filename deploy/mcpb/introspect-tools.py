#!/usr/bin/env python3
"""Print the tools/list of an MCP stdio server as JSON.

Usage: introspect-tools.py <path-to-mcp-binary> [args...]

Used by build-mcpb-universal.sh to embed the live tool definitions
(name/description/inputSchema) into the bundle manifest — registries like
Smithery score listings on tool-definition quality, and this keeps the
manifest in lockstep with the binary it ships.
"""
import json
import subprocess
import sys

proc = subprocess.Popen(
    sys.argv[1:],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
)


def call(method, params, id):
    msg = {"jsonrpc": "2.0", "id": id, "method": method, "params": params}
    proc.stdin.write((json.dumps(msg) + "\n").encode())
    proc.stdin.flush()
    return json.loads(proc.stdout.readline())


call(
    "initialize",
    {
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {"name": "introspect-tools", "version": "0"},
    },
    1,
)
tools = call("tools/list", {}, 2)["result"]["tools"]
proc.kill()
json.dump(tools, sys.stdout, indent=2)
print()
