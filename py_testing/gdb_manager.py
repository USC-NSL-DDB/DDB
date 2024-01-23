from typing import List
from time import sleep
from utils import *
from cmd_router import CmdRouter

from gdb_session import GdbSession
    
class GdbManager:
    def __init__(self, components: List[dict]) -> None:
        self.sessions: List[GdbSession] = []

        for config in components:
            self.sessions.append(GdbSession(config))

        # self.output_handle = Thread(target=self.handle_output, args=())
        # self.output_handle.start()

        self.router = CmdRouter(self.sessions)
        # self.state_manager = StateManager(self.sessions)

        # [ s.attach_state_manager(self.state_manager) for s in self.sessions ]
        [ s.start() for s in self.sessions ]

    def write(self, cmd: str):
        self.router.send_cmd(cmd)
        # pass
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
