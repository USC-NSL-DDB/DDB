from typing import List, Optional, Set
from data_struct import SessionResponse
from response_transformer import *
from threading import Lock, Thread
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

        self.process_handle = Thread(
            target=self.process_finished_response, args=()
        )
        self.process_handle.start()
        
    @staticmethod
    def inst() -> "CmdTracker":
        with CmdTracker._lock:
            if CmdTracker._instance:
                return CmdTracker._instance
            CmdTracker._instance = CmdTracker()
            return CmdTracker._instance

    def create_cmd(self, token: Optional[str], target_sessions: Set[int]):
        if token:
            with self._lock:
                self.waiting_cmds[token] = CmdMeta(token, target_sessions)
        else:
            print("No token supplied. skip registering the cmd.")

    def recv_response(self, response: SessionResponse):
        if response.token:
            with self._lock:
                result = self.waiting_cmds[response.token].recv_response(response)
                if result:
                    del self.waiting_cmds[response.token]
                    self.finished_response.put(result)
        else:
            print("no token presented. skip.")
    
    def process_finished_response(self):
        while True:
            resp = self.finished_response.get()
            print("Start to process a grouped response.")
            # For now, just test out 1234-thread-info
            ResponseTransformer.transform(resp, PlainTransformer())
