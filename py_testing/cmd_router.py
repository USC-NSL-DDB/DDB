from typing import List
from gdb_manager import GdbSession

class CmdRouter:
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions

    def send_cmd(self, cmd: str):
        print("sending cmd through the CmdRouter...")
        
        if (cmd.strip() in [ "b", "break", "-break-insert" ]):
            self.broadcast(cmd)
        
        # if (cmd.strip() in [ ] )
        # for s in self.sessions:
        #     s.write(cmd)


    def broadcast(self, cmd: str):
        for s in self.sessions:
            s.write(cmd)
    