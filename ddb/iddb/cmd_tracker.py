import time
from typing import List, Optional, Set
from iddb.data_struct import SessionResponse
from iddb.event_loop import GlobalRunningLoop
from iddb.mtracer import GlobalTracer
from iddb.utils import CmdTokenGenerator
from iddb.response_transformer import *
from iddb.logging import logger
from threading import Lock, Thread
from queue import Queue
import asyncio


class CmdMeta(asyncio.Future):
    def __init__(self, token: str, command: str, target_sessions: Set[int], transformer: Optional[ResponseTransformer] = None):
        super().__init__(loop=GlobalRunningLoop().get_loop())
        self.token = token
        self.command = command
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
        self.outTokenToInToken: dict[str, str] = {}
        self.waiting_cmds: dict[str, CmdMeta] = {}
        self.finished_cmds: dict[str, CmdMeta] = {}
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

    def create_cmd(self, token: Optional[str],command:Optional[str], target_sessions: Set[int], transformer: Optional[ResponseTransformer] = None):
        if token:
            with self._lock:
                self.waiting_cmds[token] = CmdMeta(
                    token, command,target_sessions, transformer)
        else:
            logger.debug("No token supplied. skip registering the cmd.")
            return None
    # temporary function for mutating cmdmeta
    def patch_cmdmeta(self,token:str,cmd_meta:CmdMeta):
        assert token is not None and cmd_meta is not None
        self.waiting_cmds[token]=cmd_meta
    def get_cmdmeta(self,token:str):
        assert token is not None
        return self.waiting_cmds[token]
    def dedupToken(self,token:str):
        tokenSent=token
        while tokenSent in self.outTokenToInToken:
            tokenSent=CmdTokenGenerator.get()
        self.outTokenToInToken[tokenSent]=token
        return tokenSent
        
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
                    with GlobalTracer().tracer.start_as_current_span("process response") as span:
                        span.set_attribute("token", response.token)
                        span.set_attribute(
                            "duration", (time.time_ns()-GlobalTracer().request_times[response.token])/1e9)
                        # if no one is waiting
                        if cmd_meta.get_loop().is_running():
                            cmd_meta.get_loop().call_soon_threadsafe(cmd_meta.set_result, result)
                        token = self.outTokenToInToken[cmd_meta.token]
                        del self.waiting_cmds[response.token]
                        for cmd_response in cmd_meta.responses:
                            cmd_response.token = token
                        self.finished_cmds[token] = cmd_meta
                        self.finished_response.put(cmd_meta)
                        # self.finished_response.put(result)
        else:
            logger.debug("no token presented. skip.")
    
    def process_finished_response(self):
        while True:
            cmd_meta = self.finished_response.get()
            logger.debug("Start to process a grouped response.")
            # For now, just test out 1234-thread-info
            ResponseTransformer.transform(cmd_meta.responses, cmd_meta.transformer)
