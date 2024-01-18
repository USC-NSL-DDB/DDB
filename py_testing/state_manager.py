from typing import List
from gdb_manager import GdbSession
from uuid import uuid4
from enum import Enum

class SessionStatus(Enum):
    INIT = 1
    STOPPED = 2
    RUNNING = 3

class SessionMeta:
    def __init__(self) -> None:
        self.sid = uuid4()
        self.status = SessionStatus.INIT

class StateManager:
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions
        # self.meta = [ for s in self.sessions ]
        pass

    def update():
        pass