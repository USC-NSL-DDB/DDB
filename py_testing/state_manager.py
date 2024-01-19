from typing import List
# from gdb_manager import GdbSession
from uuid import uuid4, UUID
from enum import Enum

class ThreadStatus(Enum):
    INIT = 1
    STOPPED = 2
    RUNNING = 3

class SessionMeta:
    def __init__(self, sid: UUID, tag: str) -> None:
        self.tag = tag
        self.sid = sid
        self.t_status: dict[int, ThreadStatus] = []
        # self.status = ThreadStatus.INIT
        # self.

    def update_t_status(self, tid: int, new_status: ThreadStatus):
        self.t_status[tid] = new_status

class StateManager:
    def __init__(self, sessions: List["GdbSession"]) -> None:
        self.sessions = sessions
        self.meta = { s.sid: s.meta for s in self.sessions }

    def update_t_status(self, sid: UUID, tid: int, new_status: ThreadStatus):
        self.meta[sid].update_t_status(tid, new_status)
