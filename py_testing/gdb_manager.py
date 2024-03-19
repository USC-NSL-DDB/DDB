from typing import List
from time import sleep
from gdbserver_starter import SSHRemoteServerCred, SSHRemoteSeverClient
from state_manager import StateManager
from utils import *
from cmd_router import CmdRouter

from gdb_session import GdbMode, GdbSession, GdbSessionConfig, StartMode
    
class GdbManager:
    def __init__(self, sessionConfigs: List[GdbSessionConfig]) -> None:
        self.sessions: List[GdbSession] = []

        for config in sessionConfigs:
            self.sessions.append(GdbSession(config))

        self.router = CmdRouter(self.sessions)
        self.state_mgr = StateManager.inst()

        [ s.start() for s in self.sessions ]

    def write(self, cmd: str):
        # if cmd.strip() and cmd.split()[0] == "session":
        #     selection = int(cmd.split()[1])
        #     self.state_mgr.set_current_session(selection)
        #     print(f"selected session {self.state_mgr.get_current_session()}.")
        # else:
        self.router.send_cmd(cmd)
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
    #                 # print(f"{meta} {output}")
    #                 mi_print(output, meta)
    #         sleep(0.1)

    def cleanup(self):
        print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
