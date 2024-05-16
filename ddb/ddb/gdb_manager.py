import asyncio
from typing import List, Optional
from time import sleep
from ddb.gdbserver_starter import SSHRemoteServerCred, SSHRemoteServerClient
from ddb.state_manager import StateManager
from ddb.utils import *
from ddb.cmd_router import CmdRouter

from ddb.gdb_session import GdbMode, GdbSession, GdbSessionConfig, StartMode
from ddb.logging import logger

class GdbManager:
    def __init__(self, sessionConfigs: List[GdbSessionConfig], prerun_cmds: Optional[List[dict]] = None) -> None:
        self.sessions: List[GdbSession] = []

        for config in sessionConfigs:
            self.sessions.append(GdbSession(config))

        self.router = CmdRouter(self.sessions)
        self.state_mgr = StateManager.inst()

        [ s.start(prerun_cmds) for s in self.sessions ]

    def write(self, cmd: str):
        # if cmd.strip() and cmd.split()[0] == "session":
        #     selection = int(cmd.split()[1])
        #     self.state_mgr.set_current_session(selection)
        #     dev_print(f"selected session {self.state_mgr.get_current_session()}.")
        # else:
        #asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), self.router.loop).result()
        asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), self.router.event_loop_thread.loop)
        # for s in self.sessions:
        #     s.write(cmd)

        # responses = []
        # for session in self.sessions:
        #     resp = session.write(cmd)
        #     responses.append(resp)

    # def handle_output(self):
    #     while True:
    #         for s in self.sessions:
    #             output = s.deque_mi_output()
    #             if output:
    #                 meta = s.get_meta_str()
    #                 # dev_print(f"{meta} {output}")
    #                 mi_print(output, meta)
    #         sleep(0.1)

    def cleanup(self):
        dev_print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
