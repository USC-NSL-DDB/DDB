from typing import List, Optional, Set
from data_struct import SessionResponse
from response_transformer import *
from threading import Lock, Thread
from queue import Queue
import asyncio
class CmdMeta(asyncio.Future):
    def __init__(self, token: str, target_sessions: Set[int], transformer: Optional[ResponseTransformer] = None):
        super().__init__()
        self.token = token
        self.target_sessions = target_sessions
        self.finished_sessions: Set[int] = set()
        self.responses: List[SessionResponse] = []
        self.transformer = transformer if transformer else PlainTransformer()
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
        self.finished_response: Queue[CmdMeta] = Queue(maxsize=0)

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

    def create_cmd(self, token: Optional[str], target_sessions: Set[int], transformer: Optional[ResponseTransformer] = None):
        if token:
            with self._lock:
                if token in self.waiting_cmds:
                    print(f"Token {token} already exists. Skip registering the cmd.")
                    return
                print("Creating a new cmd.")
                print("token:", token)
                print("target_sessions:", target_sessions)
                self.waiting_cmds[token] = CmdMeta(token, target_sessions, transformer)
        else:
            print("No token supplied. skip registering the cmd.")
# send a commnad-> get a future object, waiting for it to be resolved -
#bactrace
#get-remote-bt(get metadata)
#swith to its parent

#swtich back
    def recv_response(self, response: SessionResponse):
        if response.token:
            with self._lock:
                cmd_meta = self.waiting_cmds[response.token]
                result = cmd_meta.recv_response(response)
                if result:
                    print("Command Result Handling finished")
                    print(cmd_meta)
                    # if no one is waiting
                    if cmd_meta.get_loop().is_running():
                        cmd_meta.get_loop().call_soon_threadsafe(cmd_meta.set_result, result)
                    self.finished_response.put(cmd_meta)
                    print(cmd_meta)
                    del self.waiting_cmds[response.token]
                    print(cmd_meta, id(cmd_meta))
                    # self.finished_response.put(result)
        else:
            print("no token presented. skip.")
    
    def process_finished_response(self):
        while True:
            cmd_meta = self.finished_response.get()
            print("Start to process a grouped response.")
            # For now, just test out 1234-thread-info
            ResponseTransformer.transform(cmd_meta.responses, cmd_meta.transformer)
