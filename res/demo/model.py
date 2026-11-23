"""A scripted OpenAI-compatible chat-completions endpoint for recording res/demo.gif.

It answers the two questions res/demo.tape asks, the same way on every run. The second answer
is built from the `atuin_history` result the client sends back, so the recording shows the
tool really ran.
"""

import json
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

THINK_SECONDS = 0.8
WORD_SECONDS = 0.04


def chunk(delta, finish=None):
    body = {
        "id": "chatcmpl-demo",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": "demo",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    }
    return f"data: {json.dumps(body)}\n\n".encode()


def text_of(message):
    content = message.get("content") or ""
    if isinstance(content, str):
        return content
    return " ".join(part.get("text", "") for part in content if isinstance(part, dict))


def tool_call(name, args):
    return {
        "index": 0,
        "id": f"call_{name}",
        "type": "function",
        "function": {"name": name, "arguments": json.dumps(args)},
    }


def suggest(command, *, danger="low", danger_notes=""):
    args = {"command": command, "description": "", "confidence": "high", "danger": danger}
    if danger_notes:
        args["danger_notes"] = danger_notes
    return tool_call("suggest_command", args)


def find_large_files(_messages):
    text = "This lists files over 50 MB under the current directory, modified in the last 7 days:"
    return text, suggest("find . -type f -size +50M -mtime -7")


def arm64_build(messages):
    if messages[-1].get("role") != "tool":
        query = {"query": "docker buildx", "filter_modes": ["workspace", "global"], "limit": 5}
        return "Let me find that build in your history.", tool_call("atuin_history", query)

    found = re.search(r"docker buildx build[^\n`]*", text_of(messages[-1]))
    if not found:
        return "I couldn't find a `docker buildx` build in your history.", None

    previous = found.group(0).strip()
    command = previous.replace("--platform linux/amd64", "--platform linux/amd64,linux/arm64")
    tag = re.search(r"-t (\S+)", previous)
    image = tag.group(1) if tag else "the image"
    text = (
        f"Yesterday you pushed `{image}` for amd64 only. Build both platforms, so the same tag "
        "runs on either:"
    )
    return text, suggest(command, danger="med", danger_notes=f"Pushes to `{image}`, replacing it")


SCENES = [("50MB", find_large_files), ("arm64", arm64_build)]


def reply(messages):
    question = next(text_of(m) for m in reversed(messages) if m.get("role") == "user")
    for keyword, scene in SCENES:
        if keyword in question:
            return scene(messages)
    return "This model only knows the demo's two questions.", None


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        messages = json.loads(self.rfile.read(length)).get("messages", [])
        text, call = reply(messages)

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        time.sleep(THINK_SECONDS)
        for word in re.findall(r"\S+\s*", text):
            self.wfile.write(chunk({"role": "assistant", "content": word}))
            self.wfile.flush()
            time.sleep(WORD_SECONDS)
        if call:
            self.wfile.write(chunk({"role": "assistant", "tool_calls": [call]}))
        self.wfile.write(chunk({}, "tool_calls" if call else "stop"))
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, format, *args):  # noqa: A002 -- the base class's parameter name
        pass


if __name__ == "__main__":
    # Every interface: the server container reaches it through host.docker.internal, which is
    # the Docker bridge on Linux, not loopback.
    ThreadingHTTPServer(("0.0.0.0", int(sys.argv[1])), Handler).serve_forever()
