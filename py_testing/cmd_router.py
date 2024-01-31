from typing import List
from gdb_session import GdbSession
from cmd_tracker import CmdTracker
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
        prefix = None
        cmd_no_token = None
        cmd = cmd.strip()
        for idx, cmd_char in enumerate(cmd):
            if (not cmd_char.isdigit()) and (idx == 0):
                prefix = cmd.split()[0]
                cmd_no_token = cmd
                break
            
            if not cmd_char.isdigit():
                token = cmd[:idx].strip()
                cmd_no_token = cmd[idx:].strip()
                if len(cmd_no_token) == 0:
                    # no meaningful input
                    return
                prefix = cmd_no_token.split()[0]
                break

        # if token:
        #     CmdTracker.inst().create_cmd(token)

        cmd = f"{cmd}\n"
            
        # prefix = cmd.split()[0]
        # if prefix.isdigit():
        #     token = prefix
        #     prefix = cmd.split()[1]
        # prefix = prefix.strip()
        
        if (prefix in [ "b", "break", "-break-insert" ]):
            self.broadcast(cmd)
        elif (prefix in [ "run", "r", "-exec-run" ]):
            self.broadcast(cmd)
        elif (prefix in [ "list" ]):
            # self.send_to_first(cmd)
            self.send_to_current_session(cmd)
        elif (prefix in [ "c", "continue", "-exec-continue" ]):
            self.send_to_current_session(cmd)
        elif (prefix in [ "-thread-info" ]):
            target_s_ids = set()
            for s in self.sessions:
                target_s_ids.add(s.sid)
            CmdTracker.inst().create_cmd(token, target_s_ids)
            self.broadcast(cmd)
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
    