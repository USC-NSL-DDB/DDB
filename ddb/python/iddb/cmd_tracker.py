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

    def recv_response(self, response: SessionResponse) -> Optional[List[SessionResponse]]:
        self.finished_sessions.add(response.sid)
        self.responses.append(response)

        if self.__is_finished():
            return self.responses
        return None

    def __is_finished(self) -> bool:
        return self.target_sessions == self.finished_sessions
    
    def is_finished(self) -> bool:
        return self.__is_finished()

class CmdTracker:
    _instance: "CmdTracker" = None
    
    def __init__(self) -> None:
        self._lock = asyncio.Lock()
        self.outTokenToInToken: dict[str, str] = {}
        self.waiting_cmds: dict[str, CmdMeta] = {}
        self.finished_cmds: dict[str, CmdMeta] = {}
        self.response_queue: asyncio.Queue = asyncio.Queue()
        
        # Start processing task
        loop = GlobalRunningLoop().get_loop()
        loop.create_task(self.process_finished_response())
        
    @staticmethod
    def inst() -> "CmdTracker":
        if CmdTracker._instance:
            return CmdTracker._instance
        CmdTracker._instance = CmdTracker()
        return CmdTracker._instance

    async def create_cmd(self, token: Optional[str], command: Optional[str], target_sessions: Set[int], transformer: Optional[ResponseTransformer] = None):
        if token:
            async with self._lock:
                self.waiting_cmds[token] = CmdMeta(
                    token, command, target_sessions, transformer
                )
        else:
            logger.debug("No token supplied. skip registering the cmd.")
            return None

    def patch_cmdmeta(self, token: str, cmd_meta: CmdMeta):
        assert token is not None and cmd_meta is not None
        self.waiting_cmds[token] = cmd_meta

    def get_cmdmeta(self, token: str):
        assert token is not None
        return self.waiting_cmds[token]

    def dedupToken(self, token: str):
        tokenSent = token
        while tokenSent in self.outTokenToInToken:
            tokenSent = CmdTokenGenerator.get()
        self.outTokenToInToken[tokenSent] = token
        return tokenSent

    async def recv_response(self, response: SessionResponse):
        if response.token:
            async with self._lock:
                try:
                    cmd_meta = self.waiting_cmds.get(response.token)
                    result = cmd_meta.recv_response(response)
                    if result:
                        if cmd_meta.command in GlobalTracer().command_history:
                            GlobalTracer().command_history[cmd_meta.command]["finish"] = time.time_ns()
                        
                        token = self.outTokenToInToken[cmd_meta.token]
                        del self.waiting_cmds[response.token]
                        
                        for cmd_response in cmd_meta.responses:
                            cmd_response.token = token
                            
                        self.finished_cmds[token] = cmd_meta
                        await self.response_queue.put(cmd_meta)
                        
                        cmd_meta.set_result(result)
                    else:
                        logger.debug("no token presented. skip.")
                except Exception as e:
                    logger.error(f"Error when processing response: {e}")

    async def process_finished_response(self):
        while True:
            try:
                cmd_meta = await self.response_queue.get()
                logger.debug("Start to process a grouped response.")
                ResponseTransformer.transform(cmd_meta.responses, cmd_meta.transformer)
                self.response_queue.task_done()
            except Exception as e:
                logger.error(f"Error when processing response: {e}")
                break
