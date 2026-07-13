#!/usr/bin/env python3
"""Send exactly one request to the ServiceWeaver socialnet HTTP frontend."""

from __future__ import annotations

import argparse
import struct
import urllib.request


PATHS = {
    "read-user-timeline": "/read_user_timeline",
    "read-home-timeline": "/read_home_timeline",
    "follow": "/follow",
    "get-followers": "/get_followers",
    "login": "/login",
}


def encoded_string(value: str) -> bytes:
    raw = value.encode()
    return struct.pack("<I", len(raw)) + raw


def payload(args: argparse.Namespace) -> bytes:
    if args.request in {"read-user-timeline", "read-home-timeline"}:
        return struct.pack("<qqq", args.user_id, args.start, args.stop)
    if args.request == "follow":
        return struct.pack("<qq", args.user_id, args.followee_id)
    if args.request == "get-followers":
        return struct.pack("<q", args.user_id)
    if args.request == "login":
        return encoded_string(args.username) + encoded_string(args.password)
    raise AssertionError(args.request)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--addr", required=True, help="Base URL of the SocialNet API")
    parser.add_argument("--request", choices=sorted(PATHS), required=True)
    parser.add_argument("--user-id", type=int, default=1)
    parser.add_argument("--followee-id", type=int, default=2)
    parser.add_argument("--start", type=int, default=0)
    parser.add_argument("--stop", type=int, default=1)
    parser.add_argument("--username", default="username_1")
    parser.add_argument("--password", default="password123")
    parser.add_argument("--timeout", type=float, default=120.0)
    args = parser.parse_args()

    request = urllib.request.Request(
        args.addr.rstrip("/") + PATHS[args.request],
        data=payload(args),
        method="POST",
        headers={"Content-Type": "application/custom"},
    )
    with urllib.request.urlopen(request, timeout=args.timeout) as response:
        print(f"HTTP {response.status} {PATHS[args.request]}")


if __name__ == "__main__":
    main()
