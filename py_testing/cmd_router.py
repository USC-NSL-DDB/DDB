from typing import List
from gdb_session import GdbSession
from state_manager import StateManager

class CmdRouter:
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions
        self.state_mgr = StateManager.inst()

    def send_cmd(self, cmd: str):
        print("sending cmd through the CmdRouter...")

        if len(cmd.strip()) == 0:
            # special case of no meaningful command
            return

        token = None
        prefix = cmd.split()[0]
        if prefix.isdigit():
            token = prefix
            prefix = cmd.split()[1]
        prefix = prefix.strip()
        
        if (prefix in [ "b", "break", "-break-insert" ]):
            self.broadcast(cmd)
        elif (prefix in [ "run", "r", "-exec-run" ]):
            self.broadcast(cmd)
        elif (prefix in [ "list" ]):
            # self.send_to_first(cmd)
            self.send_to_current_session(cmd)
        elif (prefix in [ "c", "continue", "-exec-continue" ]):
            self.send_to_current_session(cmd)
        else:
            self.send_to_current_session(cmd)
            # self.broadcast(cmd)
        
        
        # if (cmd.strip() in [ ] )
        # for s in self.sessions:
        #     s.write(cmd)

    def send_to_current_session(self, cmd: str):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            print("use session #sno to select session.")
            return
        [ s.write(cmd) for s in self.sessions if s.sid == curr_session ]

    def broadcast(self, cmd: str):
        for s in self.sessions:
            s.write(cmd)

    # def send_to_random_one(self, cmd: str):
        

    def send_to_first(self, cmd: str):
        self.sessions[0].write(cmd)
    