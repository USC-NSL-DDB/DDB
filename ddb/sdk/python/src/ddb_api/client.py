"""Bounded standard-library transport for the DDB API v2 ProtoJSON binding."""

from __future__ import annotations

import json
import threading
import time
import uuid
from collections.abc import Callable, Iterator, Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any, cast
from urllib.error import HTTPError, URLError
from urllib.parse import SplitResult, urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from .generated import types as t
from .generated.contract import METHODS, PAGINATED_METHODS

JsonObject = dict[str, Any]
Opener = Callable[..., Any]

_TERMINAL_STATES = {
    "OPERATION_STATE_COMPLETED",
    "OPERATION_STATE_FAILED",
    "OPERATION_STATE_CANCELLED",
}
_REHYDRATE_CODES = {"DDB_ERROR_CODE_REPLAY_GAP", "DDB_ERROR_CODE_EXPIRED"}
_RETRYABLE_CODES = {"DDB_ERROR_CODE_NOT_READY", "DDB_ERROR_CODE_UNAVAILABLE"}


class _RejectRedirects(HTTPRedirectHandler):
    def redirect_request(
        self,
        _request: Request,
        _file_pointer: Any,
        _code: int,
        _message: str,
        _headers: Any,
        _new_url: str,
    ) -> None:
        return None


class DdbClientError(Exception):
    """Base class for public SDK errors."""


class ApiError(DdbClientError):
    """A stable typed error returned by DDB."""

    def __init__(self, status: int, detail: t.DdbError) -> None:
        self.status = status
        self.detail = detail
        super().__init__(detail.get("message") or f"DDB returned HTTP {status}")


class HttpError(DdbClientError):
    """An HTTP response that did not contain a valid v2 error envelope."""

    def __init__(self, status: int, body: str) -> None:
        self.status = status
        self.body = body
        super().__init__(f"DDB returned HTTP {status} without a valid v2 error envelope")

    def is_api_version_unavailable(self) -> bool:
        """True only for an explicit untyped HTTP 404 on the v2 route."""

        return self.status == 404


class TransportError(DdbClientError):
    """Connection, TLS, timeout, or response-I/O failure."""


class ProtocolError(DdbClientError):
    """Invalid configuration, bounds violation, or malformed wire data."""


class ClientClosedError(DdbClientError):
    """The client was closed while work was active or before a new request."""


class StreamEndedError(DdbClientError):
    """A reconnectable event stream ended without a terminal protocol event."""


@dataclass(frozen=True, slots=True)
class RetryPolicy:
    initial_backoff: float = 0.1
    max_backoff: float = 5.0
    max_attempts: int | None = None

    def validate(self) -> None:
        if self.initial_backoff <= 0 or self.max_backoff <= 0:
            raise ProtocolError("retry backoffs must be greater than zero")
        if self.initial_backoff > self.max_backoff:
            raise ProtocolError("initial_backoff must not exceed max_backoff")
        if self.max_attempts is not None and self.max_attempts <= 0:
            raise ProtocolError("max_attempts must be greater than zero")


class DdbClient:
    """Synchronous public DDB client with reconnecting iterator helpers."""

    def __init__(
        self,
        endpoint: str,
        bearer_token: str | None = None,
        *,
        connect_timeout: float = 3.0,
        request_timeout: float = 10.0,
        max_request_bytes: int = 4 * 1024 * 1024,
        max_response_bytes: int = 16 * 1024 * 1024,
        max_stream_line_bytes: int = 4 * 1024 * 1024,
        opener: Opener | None = None,
    ) -> None:
        self._endpoint = _normalize_endpoint(endpoint)
        if bearer_token is not None and not bearer_token.strip():
            raise ProtocolError("bearer_token must not be empty")
        self._bearer_token = bearer_token
        self._connect_timeout = _positive(connect_timeout, "connect_timeout")
        self._request_timeout = _positive(request_timeout, "request_timeout")
        self._max_request_bytes = _positive_int(max_request_bytes, "max_request_bytes")
        self._max_response_bytes = _positive_int(max_response_bytes, "max_response_bytes")
        self._max_stream_line_bytes = _positive_int(
            max_stream_line_bytes, "max_stream_line_bytes"
        )
        self._opener = opener or build_opener(_RejectRedirects()).open
        self._lock = threading.Lock()
        self._responses: set[Any] = set()
        self._closed = threading.Event()

    def __repr__(self) -> str:
        return (
            f"DdbClient(endpoint={self._endpoint!r}, "
            f"authenticated={self._bearer_token is not None}, closed={self.closed})"
        )

    @property
    def closed(self) -> bool:
        return self._closed.is_set()

    def __enter__(self) -> DdbClient:
        self._ensure_open()
        return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        self.close()

    def close(self) -> None:
        if self._closed.is_set():
            return
        self._closed.set()
        with self._lock:
            responses = tuple(self._responses)
            self._responses.clear()
        for response in responses:
            try:
                response.close()
            except Exception:
                pass

    def call(
        self,
        method: str,
        request: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> JsonObject:
        """Invoke any generated unary RPC by `Service.Method` name."""

        self._ensure_open()
        spec = METHODS.get(method)
        if spec is None:
            raise ProtocolError(f"unknown DDB method {method!r}")
        if spec.server_streaming:
            raise ProtocolError(f"{method} is streaming")
        request_timeout = _positive(timeout or self._request_timeout, "timeout")
        body = self._prepare(method, request or {}, request_timeout)
        response = self._open(spec.path, body, request_timeout)
        try:
            status = _status(response)
            payload = _read_bounded(response, self._max_response_bytes)
            if status < 200 or status >= 300:
                raise _decode_error(status, payload)
            return _parse_object(payload, f"{method} response")
        finally:
            self._release(response)

    def stream(
        self,
        method: str,
        request: Mapping[str, Any] | None = None,
    ) -> Iterator[JsonObject]:
        """Open one generated server-streaming RPC and yield bounded NDJSON objects."""

        self._ensure_open()
        spec = METHODS.get(method)
        if spec is None:
            raise ProtocolError(f"unknown DDB method {method!r}")
        if not spec.server_streaming:
            raise ProtocolError(f"{method} is not streaming")
        body = self._prepare(method, request or {}, self._request_timeout)
        response = self._open(spec.path, body, self._connect_timeout)
        try:
            status = _status(response)
            if status < 200 or status >= 300:
                raise _decode_error(
                    status, _read_bounded(response, self._max_response_bytes)
                )
            while True:
                try:
                    line = response.readline(self._max_stream_line_bytes + 2)
                except Exception as error:
                    if self.closed:
                        raise ClientClosedError("DDB client is closed") from error
                    raise TransportError("DDB stream read failed") from error
                if not line:
                    return
                line = line.rstrip(b"\r\n")
                if len(line) > self._max_stream_line_bytes:
                    raise ProtocolError(
                        f"stream line exceeds the {self._max_stream_line_bytes}-byte bound"
                    )
                if not line:
                    continue
                yield _parse_object(line, "NDJSON stream line")
        finally:
            self._release(response)

    def handshake(self) -> tuple[t.ServerInfo, t.Capabilities]:
        server = cast(t.GetServerInfoResponse, self.call("DebuggerService.GetServerInfo"))
        capabilities_response = cast(
            t.GetCapabilitiesResponse,
            self.call("DebuggerService.GetCapabilities"),
        )
        server_info = server.get("serverInfo")
        capabilities = capabilities_response.get("capabilities")
        if (
            not server_info
            or not server_info.get("serverInstanceId")
            or "v2" not in server_info.get("apiVersions", [])
        ):
            raise ProtocolError("server does not advertise API v2")
        if (
            not capabilities
            or capabilities.get("apiVersion") != "v2"
            or not capabilities.get("schemaVersion")
            or capabilities.get("serverInstanceId")
            != server_info.get("serverInstanceId")
        ):
            raise ProtocolError("capabilities do not match the negotiated v2 server")
        return server_info, capabilities

    def get_snapshot(
        self, request: t.GetSnapshotRequest | None = None
    ) -> t.GetSnapshotResponse:
        return cast(
            t.GetSnapshotResponse,
            self.call("DebuggerService.GetSnapshot", request),
        )

    def execute(self, request: t.ExecuteRequest) -> t.OperationAdmissionResponse:
        return cast(
            t.OperationAdmissionResponse,
            self.call("DebuggerControlService.Execute", request),
        )

    def create_breakpoint(
        self, request: t.CreateBreakpointRequest
    ) -> t.OperationAdmissionResponse:
        return cast(
            t.OperationAdmissionResponse,
            self.call("DebuggerControlService.CreateBreakpoint", request),
        )

    def delete_breakpoint(
        self, request: t.DeleteBreakpointRequest
    ) -> t.OperationAdmissionResponse:
        return cast(
            t.OperationAdmissionResponse,
            self.call("DebuggerControlService.DeleteBreakpoint", request),
        )

    def run_distributed_backtrace(
        self, request: t.RunDistributedBacktraceRequest
    ) -> t.OperationAdmissionResponse:
        return cast(
            t.OperationAdmissionResponse,
            self.call("DebuggerControlService.RunDistributedBacktrace", request),
        )

    def collect(
        self,
        method: str,
        request: Mapping[str, Any] | None = None,
        *,
        max_items: int = 10_000,
    ) -> list[JsonObject]:
        """Collect one known paginated method with token-loop and size guards."""

        _positive_int(max_items, "max_items")
        items_field = PAGINATED_METHODS.get(method)
        if items_field is None:
            raise ProtocolError(f"{method!r} is not a generated paginated method")
        original = dict(request or {})
        original_page = original.get("page")
        page_size = (
            original_page.get("pageSize")
            if isinstance(original_page, Mapping)
            else None
        )
        token = (
            original_page.get("pageToken")
            if isinstance(original_page, Mapping)
            else None
        )
        seen: set[str] = set()
        result: list[JsonObject] = []
        while True:
            page: JsonObject = {}
            if page_size is not None:
                page["pageSize"] = page_size
            if token is not None:
                page["pageToken"] = token
            response = self.call(method, {**original, "page": page})
            items = response.get(items_field)
            if not isinstance(items, list) or not all(isinstance(item, dict) for item in items):
                raise ProtocolError(f"{method} omitted {items_field}")
            if len(result) + len(items) > max_items:
                raise ProtocolError(f"{method} exceeded the {max_items}-item bound")
            result.extend(items)
            info = response.get("page")
            token = info.get("nextPageToken") if isinstance(info, dict) else None
            if token is None:
                return result
            if not isinstance(token, str) or not token or token in seen:
                raise ProtocolError(f"{method} returned an invalid continuation token")
            seen.add(token)

    def wait_operation(
        self,
        operation_id: str,
        *,
        timeout: float = 10.0,
        poll_interval: float = 0.05,
    ) -> t.Operation:
        if not operation_id:
            raise ProtocolError("operation_id must not be empty")
        timeout = _positive(timeout, "timeout")
        poll_interval = _positive(poll_interval, "poll_interval")
        deadline = time.monotonic() + timeout
        while True:
            response = cast(
                t.GetOperationResponse,
                self.call(
                    "DebuggerService.GetOperation", {"operationId": operation_id}
                ),
            )
            operation = response.get("operation")
            if not operation or not operation.get("operationId"):
                raise ProtocolError("GetOperation omitted operation")
            if operation.get("state") in _TERMINAL_STATES:
                return operation
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProtocolError(
                    f"operation {operation_id} did not complete before timeout"
                )
            self._closed.wait(min(poll_interval, remaining))
            self._ensure_open()

    def subscribe_state_events(
        self,
        request: t.SubscribeStateEventsRequest | None = None,
        *,
        retry: RetryPolicy = RetryPolicy(),
    ) -> Iterator[t.StateEvent]:
        for event in self._reconnecting_stream(
            "DdbEventService.SubscribeStateEvents", request or {}, retry
        ):
            yield cast(t.StateEvent, event)

    def subscribe_output(
        self,
        request: t.SubscribeOutputRequest | None = None,
        *,
        retry: RetryPolicy = RetryPolicy(),
    ) -> Iterator[t.OutputEvent]:
        for event in self._reconnecting_stream(
            "DdbEventService.SubscribeOutput", request or {}, retry
        ):
            yield cast(t.OutputEvent, event)

    def state_sync(
        self,
        request: t.GetSnapshotRequest | None = None,
        *,
        retry: RetryPolicy = RetryPolicy(),
    ) -> Iterator[tuple[str, t.Snapshot | t.StateEvent]]:
        """Yield a snapshot followed by its replay-safe live state suffix."""

        while not self.closed:
            response = self.get_snapshot(request)
            snapshot = response.get("snapshot")
            if (
                not snapshot
                or not snapshot.get("serverInstanceId")
                or not snapshot.get("stateEventCursor")
            ):
                raise ProtocolError("GetSnapshot omitted synchronization metadata")
            yield "snapshot", snapshot
            try:
                for event in self.subscribe_state_events(
                    {"afterCursor": snapshot["stateEventCursor"]}, retry=retry
                ):
                    yield "event", event
                return
            except DdbClientError as error:
                if not requires_rehydration(error):
                    raise

    def _reconnecting_stream(
        self,
        method: str,
        request: Mapping[str, Any],
        retry: RetryPolicy,
    ) -> Iterator[JsonObject]:
        retry.validate()
        after_cursor = request.get("afterCursor")
        attempts = 0
        while not self.closed:
            current = dict(request)
            if after_cursor is not None:
                current["afterCursor"] = after_cursor
            try:
                for event in self.stream(method, current):
                    attempts = 0
                    cursor = event.get("cursor")
                    if isinstance(cursor, dict):
                        after_cursor = cursor
                    yield event
                raise StreamEndedError("DDB event stream ended")
            except DdbClientError as error:
                if self.closed:
                    return
                if requires_rehydration(error) or not is_retryable(error):
                    raise
                attempts += 1
                if retry.max_attempts is not None and attempts > retry.max_attempts:
                    raise
                delay = min(
                    retry.max_backoff,
                    retry.initial_backoff * 2 ** min(attempts - 1, 20),
                )
                self._closed.wait(delay)

    def _prepare(
        self, method: str, request: Mapping[str, Any], timeout: float
    ) -> bytes:
        if not isinstance(request, Mapping):
            raise ProtocolError(f"{method} request must be a mapping")
        prepared = dict(request)
        supplied_context = prepared.get("context")
        if supplied_context is not None and not isinstance(supplied_context, Mapping):
            raise ProtocolError("request context must be a mapping")
        context = dict(supplied_context or {})
        if not context.get("deadline"):
            deadline = datetime.now(timezone.utc) + timedelta(seconds=timeout)
            context["deadline"] = deadline.isoformat(timespec="milliseconds").replace(
                "+00:00", "Z"
            )
        if _is_mutation(method) and not context.get("idempotencyKey"):
            context["idempotencyKey"] = str(uuid.uuid4())
        prepared["context"] = context
        try:
            body = json.dumps(
                prepared, separators=(",", ":"), ensure_ascii=False
            ).encode("utf-8")
        except (TypeError, ValueError) as error:
            raise ProtocolError("request is not JSON serializable") from error
        if len(body) > self._max_request_bytes:
            raise ProtocolError(
                f"request exceeds the {self._max_request_bytes}-byte bound"
            )
        return body

    def _open(self, path: str, body: bytes, timeout: float) -> Any:
        self._ensure_open()
        headers = {
            "Accept": "application/json, application/x-ndjson",
            "Content-Type": "application/json",
        }
        if self._bearer_token is not None:
            headers["Authorization"] = f"Bearer {self._bearer_token}"
        request = Request(
            self._endpoint + path.lstrip("/"),
            data=body,
            headers=headers,
            method="POST",
        )
        try:
            response = self._opener(request, timeout=timeout)
        except HTTPError as error:
            try:
                payload = _read_bounded(error, self._max_response_bytes)
            finally:
                error.close()
            raise _decode_error(error.code, payload) from None
        except (URLError, OSError, TimeoutError) as error:
            if self.closed:
                raise ClientClosedError("DDB client is closed") from error
            raise TransportError("DDB transport failed") from error
        with self._lock:
            if self.closed:
                try:
                    response.close()
                finally:
                    raise ClientClosedError("DDB client is closed")
            self._responses.add(response)
        return response

    def _release(self, response: Any) -> None:
        with self._lock:
            self._responses.discard(response)
        try:
            response.close()
        except Exception:
            pass

    def _ensure_open(self) -> None:
        if self.closed:
            raise ClientClosedError("DDB client is closed")


def _normalize_endpoint(endpoint: str) -> str:
    try:
        parsed = urlsplit(endpoint)
    except ValueError as error:
        raise ProtocolError("invalid DDB endpoint") from error
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ProtocolError("DDB endpoint must use http or https and include a host")
    if parsed.username or parsed.password:
        raise ProtocolError("DDB endpoint must not contain credentials")
    path = parsed.path.rstrip("/") + "/"
    clean = SplitResult(parsed.scheme, parsed.netloc, path, "", "")
    return urlunsplit(clean)


def _positive(value: float, name: str) -> float:
    if not isinstance(value, (int, float)) or isinstance(value, bool) or value <= 0:
        raise ProtocolError(f"{name} must be greater than zero")
    return float(value)


def _positive_int(value: int, name: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise ProtocolError(f"{name} must be a positive integer")
    return value


def _is_mutation(method: str) -> bool:
    return method.startswith("DebuggerControlService.") or method == "DdbAdminService.Shutdown"


def _status(response: Any) -> int:
    status = getattr(response, "status", None)
    if status is None:
        status = response.getcode()
    if not isinstance(status, int):
        raise ProtocolError("HTTP response omitted a numeric status")
    return status


def _read_bounded(response: Any, limit: int) -> bytes:
    declared = response.headers.get("Content-Length") if response.headers else None
    if declared is not None:
        try:
            if int(declared) > limit:
                raise ProtocolError(f"response exceeds the {limit}-byte bound")
        except ValueError as error:
            raise ProtocolError("response has an invalid Content-Length") from error
    chunks: list[bytes] = []
    length = 0
    while True:
        try:
            chunk = response.read(min(64 * 1024, limit - length + 1))
        except Exception as error:
            raise TransportError("DDB response read failed") from error
        if not chunk:
            return b"".join(chunks)
        length += len(chunk)
        if length > limit:
            raise ProtocolError(f"response exceeds the {limit}-byte bound")
        chunks.append(chunk)


def _parse_object(payload: bytes, label: str) -> JsonObject:
    try:
        value = json.loads(payload or b"{}")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ProtocolError(f"{label} is not valid JSON") from error
    if not isinstance(value, dict):
        raise ProtocolError(f"{label} is not a JSON object")
    return value


def _decode_error(status: int, payload: bytes) -> DdbClientError:
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError):
        value = None
    if (
        isinstance(value, dict)
        and isinstance(value.get("code"), str)
        and value["code"] != "DDB_ERROR_CODE_UNSPECIFIED"
        and isinstance(value.get("message"), str)
        and value["message"].strip()
    ):
        return ApiError(status, cast(t.DdbError, value))
    return HttpError(status, payload.decode("utf-8", "replace")[:512])


def requires_rehydration(error: BaseException) -> bool:
    return isinstance(error, ApiError) and error.detail.get("code") in _REHYDRATE_CODES


def is_retryable(error: BaseException) -> bool:
    if isinstance(error, (TransportError, StreamEndedError)):
        return True
    if isinstance(error, ApiError):
        return bool(error.detail.get("retryable")) or error.detail.get("code") in _RETRYABLE_CODES
    return isinstance(error, HttpError) and error.status in {429, 502, 503, 504}
