from typing import List, Optional, Set
from data_struct import SessionResponse
from threading import Lock
from queue import Queue

class CmdMeta:
    def __init__(self, token: str, target_sessions: Set[int]):
        self.token = token
        self.target_sessions = target_sessions
        self.finished_sessions: Set[int] = set()
        self.responses: List[SessionResponse] = []
        self.lock = Lock()

    def recv_response(self, response: SessionResponse) -> Optional[List[SessionResponse]]:
        with self.lock:
            self.finished_sessions.add(response.sid)
            self.responses.append(response)

            if self.__is_finished():
                return self.responses
        return None

    def __is_finished(self) -> bool:
        return self.target_sessions == self.finished_sessions
    
    def is_finished(self) -> bool:
        with self.lock:
            return self.__is_finished()

class CmdTracker:
    _instance: "CmdTracker" = None
    _lock = Lock()
    
    def __init__(self) -> None:
        self._lock = Lock()
        self.waiting_cmds: dict[str, CmdMeta] = {}
        self.finished_response: Queue[List[SessionResponse]] = Queue(maxsize=0)
        
    @staticmethod
    def inst() -> "CmdTracker":
        with CmdTracker._lock:
            if CmdTracker._instance:
                return CmdTracker._instance
            CmdTracker._instance = CmdTracker()
            return CmdTracker._instance

    def create_cmd(self, token: Optional[str]):
        if token:
            with self._lock:
                self.waiting_cmds[token] = CmdMeta(token)
        else:
            print("No token supplied. skip registering the cmd.")

    def recv_response(self, response: SessionResponse):
        if response.token:
            with self._lock:
                result = self.waiting_cmds[response.token].recv_response(response)
                if result:
                    self.finished_response.put(result)
        else:
            print("no token presented. skip.")
