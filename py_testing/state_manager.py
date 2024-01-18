from typing import List
from gdb_manager import GdbSession
from uuid import uuid4

class SessionMeta:
    def __init__(self) -> None:
        pass

class StateManager:
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions
        # self.meta = [ for s in self.sessions ]
        pass

    def update():
        pass