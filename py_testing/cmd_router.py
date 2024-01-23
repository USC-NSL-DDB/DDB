from typing import List
from gdb_session import GdbSession

class CmdRouter:
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions

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
        elif (prefix in [ "run", "r" ]):
            self.broadcast(cmd)
        elif (prefix in [ "list" ]):
            self.send_to_first(cmd)
        else:
            self.broadcast(cmd)
        
        
        # if (cmd.strip() in [ ] )
        # for s in self.sessions:
        #     s.write(cmd)


    def broadcast(self, cmd: str):
        for s in self.sessions:
            s.write(cmd)

    def send_to_first(self, cmd: str):
        self.sessions[0].write(cmd)
    