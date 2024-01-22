from typing import List
# from gdb_manager import GdbSession
from uuid import uuid4, UUID
from enum import Enum
from threading import Lock

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
    _store: "StateManager" = None
    _lock = Lock()

    def __init__(self) -> None:
        pass

    @staticmethod
    def inst() -> "StateManager":
        with StateManager._lock:
            if StateManager._store:
                return StateManager._store 
            StateManager._store = StateManager()
            return StateManager._store

# Eager instantiation
_ = StateManager.inst()
