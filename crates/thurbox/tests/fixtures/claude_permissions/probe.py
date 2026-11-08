"""Exercise the installed Claude CLI's permission checks against a local fake API."""

import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading


class FakeAPI(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if "count_tokens" in self.path:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"input_tokens":100}')
            return
        answered = any(
            isinstance(message.get("content"), list)
            and any(block.get("type") == "tool_result" and block.get("tool_use_id", "").startswith(self.server.tool_prefix) for block in message["content"])
            for message in request["messages"][-1:]
        )
        content = [{"type": "text", "text": "Done."}] if answered else [
            {"type": "tool_use", "id": f"{self.server.tool_prefix}{index}", "name": "Edit",
             "input": {"file_path": str(path), "old_string": "", "new_string": "probe\n"}}
            for index, path in enumerate(self.server.targets)
        ]
        stop = "end_turn" if answered else "tool_use"
        message = {"id": "msg_probe", "type": "message", "role": "assistant",
                   "model": request["model"], "content": content, "stop_reason": stop,
                   "stop_sequence": None, "usage": {"input_tokens": 100, "output_tokens": 10}}
        self.send_response(200)
        if not request.get("stream"):
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(message).encode())
            return
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        events = [("message_start", {"type": "message_start", "message": {
            **message, "content": [], "stop_reason": None}})]
        for index, block in enumerate(content):
            start = {**block, "input": {}} if block["type"] == "tool_use" else {"type": "text", "text": ""}
            delta = ({"type": "input_json_delta", "partial_json": json.dumps(block["input"])}
                     if block["type"] == "tool_use" else {"type": "text_delta", "text": block["text"]})
            events.extend([
                ("content_block_start", {"type": "content_block_start", "index": index, "content_block": start}),
                ("content_block_delta", {"type": "content_block_delta", "index": index, "delta": delta}),
                ("content_block_stop", {"type": "content_block_stop", "index": index}),
            ])
        events.extend([
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 10}}),
            ("message_stop", {"type": "message_stop"}),
        ])
        for event, data in events:
            self.wfile.write(f"event: {event}\ndata: {json.dumps(data)}\n\n".encode())


def main():
    workspace = Path.cwd()
    members = sorted(path for path in workspace.iterdir() if path.is_symlink())
    assert len(members) == 3, "expected the actual three-member launch workspace"
    with tempfile.TemporaryDirectory(dir=os.environ["PROBE_HOME"]) as scratch:
        scratch = Path(scratch)
        config = scratch / "config"
        config.mkdir()
        outside = scratch / "outside"
        outside.mkdir()
        escape = members[0] / "escape"
        escape.symlink_to(outside, target_is_directory=True)
        server = http.server.ThreadingHTTPServer(("localhost", 0), FakeAPI)
        server.targets = []
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        env = {"PATH": os.environ["PATH"], "HOME": str(scratch),
               "CLAUDE_CONFIG_DIR": str(config), "ANTHROPIC_API_KEY": "local-test-key",
               "ANTHROPIC_BASE_URL": f"http://localhost:{server.server_port}",
               "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1"}
        args = sys.argv[1:]
        try:
            for round_number in range(2):
                allowed = [member / f"{scratch.name}-probe-{round_number}.txt" for member in members]
                allowed += [member.resolve() / f"{scratch.name}-canonical-{round_number}.txt" for member in members]
                blocked = escape / f"blocked-{round_number}.txt"
                server.targets = allowed + [blocked]
                server.tool_prefix = f"toolu_round{round_number}_"
                run = subprocess.run(
                    [os.environ["THURBOX_TEST_CLAUDE"], "--bare", "-p", "--tools", "Edit",
                     "--model", "sonnet", "--output-format", "json", *args, "--", "Perform the edits."],
                    env=env, capture_output=True, text=True, timeout=20, check=True,
                )
                result = json.loads(run.stdout)
                assert all(path.exists() and path.read_text() == "probe\n" for path in allowed), result
                assert not blocked.exists(), "an outside symlink target escaped session scope"
                denied = result["permission_denials"]
                assert len(denied) == 1 and denied[0]["tool_input"]["file_path"] == str(blocked), denied
                if round_number == 0:
                    index = args.index("--session-id")
                    args[index] = "--resume"
            print(json.dumps({"passed": True, "rounds": 2, "member_edits": 12, "outside_denials": 2}))
        finally:
            server.shutdown()
            server.server_close()
            worker.join()
            escape.unlink()


if __name__ == "__main__":
    main()
