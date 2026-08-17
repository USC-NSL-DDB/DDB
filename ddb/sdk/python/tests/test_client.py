from __future__ import annotations

import io
import json
import unittest
from email.message import Message
from typing import Any

from ddb_api import ApiError, ClientClosedError, DdbClient, HttpError, ProtocolError


class Response(io.BytesIO):
    def __init__(self, body: bytes, status: int = 200, headers: dict[str, str] | None = None):
        super().__init__(body)
        self.status = status
        self.headers = Message()
        for name, value in (headers or {}).items():
            self.headers[name] = value

    def getcode(self) -> int:
        return self.status


class ClientTests(unittest.TestCase):
    def test_preserves_prefix_and_prepares_mutation_policy(self) -> None:
        observed: dict[str, Any] = {}

        def opener(request: Any, *, timeout: float) -> Response:
            observed.update(
                url=request.full_url,
                timeout=timeout,
                authorization=request.get_header("Authorization"),
                body=json.loads(request.data),
            )
            return Response(b'{"operation":{"operationId":"op_1"}}')

        client = DdbClient(
            "https://debug.example/team/a",
            "control-token",
            opener=opener,
        )
        client.execute(
            {
                "target": {"currentThread": {}},
                "action": "EXECUTION_ACTION_NEXT",
            }
        )

        self.assertEqual(
            observed["url"],
            "https://debug.example/team/a/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
        )
        self.assertEqual(observed["authorization"], "Bearer control-token")
        self.assertRegex(observed["body"]["context"]["idempotencyKey"], r"^[0-9a-f-]{36}$")
        self.assertTrue(observed["body"]["context"]["deadline"].endswith("Z"))
        self.assertNotIn("control-token", repr(client))

    def test_only_untyped_404_allows_explicit_fallback(self) -> None:
        missing = DdbClient(
            "http://127.0.0.1:1",
            opener=lambda _request, **_kwargs: Response(b'{"apiVersion":"v1"}', 404),
        )
        with self.assertRaises(HttpError) as missing_error:
            missing.call("DebuggerService.GetServerInfo")
        self.assertTrue(missing_error.exception.is_api_version_unavailable())

        typed = DdbClient(
            "http://127.0.0.1:1",
            opener=lambda _request, **_kwargs: Response(
                b'{"code":"DDB_ERROR_CODE_NOT_FOUND","message":"thread not found"}',
                404,
            ),
        )
        with self.assertRaises(ApiError) as typed_error:
            typed.call("DebuggerService.GetThread", {"threadId": "thr_missing"})
        self.assertEqual(typed_error.exception.detail["code"], "DDB_ERROR_CODE_NOT_FOUND")

    def test_collect_follows_bounded_pages(self) -> None:
        tokens: list[str | None] = []

        def opener(request: Any, **_kwargs: Any) -> Response:
            page = json.loads(request.data)["page"]
            tokens.append(page.get("pageToken"))
            if "pageToken" not in page:
                return Response(
                    b'{"sessions":[{"sessionId":"ses_1"}],"page":{"nextPageToken":"next"}}'
                )
            return Response(b'{"sessions":[{"sessionId":"ses_2"}],"page":{}}')

        client = DdbClient("http://127.0.0.1:1", opener=opener)
        sessions = client.collect(
            "DebuggerService.ListSessions",
            {"page": {"pageSize": 1}},
            max_items=2,
        )
        self.assertEqual(tokens, [None, "next"])
        self.assertEqual([session["sessionId"] for session in sessions], ["ses_1", "ses_2"])

    def test_stream_ignores_heartbeats_and_enforces_line_bound(self) -> None:
        client = DdbClient(
            "http://127.0.0.1:1",
            opener=lambda _request, **_kwargs: Response(
                b'{"text":"one"}\n\n{"text":"two"}\n'
            ),
        )
        self.assertEqual(
            [event["text"] for event in client.stream("DdbEventService.SubscribeOutput")],
            ["one", "two"],
        )

        bounded = DdbClient(
            "http://127.0.0.1:1",
            max_stream_line_bytes=8,
            opener=lambda _request, **_kwargs: Response(b'{"text":"too long"}\n'),
        )
        with self.assertRaises(ProtocolError):
            next(iter(bounded.stream("DdbEventService.SubscribeOutput")))

    def test_close_prevents_new_requests(self) -> None:
        client = DdbClient("http://127.0.0.1:1")
        client.close()
        with self.assertRaises(ClientClosedError):
            client.call("DebuggerService.GetServerInfo")


if __name__ == "__main__":
    unittest.main()
