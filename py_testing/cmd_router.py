from threading import Lock
from typing import List, Optional, Set, Union
from gdb_session import GdbSession
from cmd_tracker import CmdTracker
from counter import TSCounter
from response_transformer import ResponseTransformer, ThreadInfoReadableTransformer, ThreadInfoTransformer
from state_manager import StateManager

# A simple wrapper around counter in case any customization later
class CmdTokenGenerator:
    _sc: "CmdTokenGenerator" = None
    _lock = Lock()
    
    def __init__(self) -> None:
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "CmdTokenGenerator":
        with CmdTokenGenerator._lock:
            if CmdTokenGenerator._sc:
                return CmdTokenGenerator._sc
            CmdTokenGenerator._sc = CmdTokenGenerator()
            return CmdTokenGenerator._sc

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return CmdTokenGenerator.inst().inc()

class CmdRouter:
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = sessions
        self.state_mgr = StateManager.inst()

    def prepend_token(self, cmd: str) -> str:
        token = CmdTokenGenerator.get()
        return f"{token}{cmd}"

    def send_cmd(self, cmd: str):
        print("sending cmd through the CmdRouter...")

        if len(cmd.strip()) == 0:
            # special case of no meaningful command
            return

        if cmd[0] == ":":
            # handle private command
            self.handle_private_cmd(cmd[1:])
            return

        cmd = self.prepend_token(cmd)

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
            self.broadcast(token, cmd)
        elif (prefix in [ "run", "r", "-exec-run" ]):
            self.broadcast(token, cmd)
        elif (prefix in [ "list" ]):
            # self.send_to_first(cmd)
            self.send_to_current_session(token, cmd)
        elif (prefix in [ "c", "continue", "-exec-continue" ]):
            self.send_to_current_session(token, cmd)
        elif (prefix in [ "-thread-info" ]):
            self.broadcast(token, cmd, ThreadInfoTransformer())
        elif (prefix in [ "info" ]):
            subcmd = cmd_no_token.split()[1]
            if subcmd == "threads" or subcmd == "thread":
                self.broadcast(token, f"{token}-thread-info", ThreadInfoReadableTransformer())
        else:
            self.send_to_current_session(token, cmd)
            # self.broadcast(cmd)
        
        
        # if (cmd.strip() in [ ] )
        # for s in self.sessions:
        #     s.write(cmd)

    def send_to_current_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            print("use session #sno to select session.")
            return

        self.register_cmd(token, curr_session, transformer)
        [ s.write(cmd) for s in self.sessions if s.sid == curr_session ]

    def broadcast(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd_for_all(token, transformer)
        for s in self.sessions:
            s.write(cmd)

    # def send_to_random_one(self, cmd: str):
        

    def send_to_first(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd(token, self.sessions[0].sid, transformer) 
        self.sessions[0].write(cmd)
    
    ### Some help functions for registering cmds
    def register_cmd_for_all(self, token: Optional[str], transformer: Optional[ResponseTransformer] = None):
        target_s_ids = set()
        for s in self.sessions:
            target_s_ids.add(s.sid)
        self.register_cmd(token, target_s_ids, transformer)

    def register_cmd(self, token: Optional[str], target_sessions: Union[int, Set[int]], transformer: Optional[ResponseTransformer] = None):
        if token:
            if isinstance(target_sessions, int):
                target_sessions = { target_sessions }

            if not isinstance(target_sessions, Set):
                raise Exception("wrong argument")

            CmdTracker.inst().create_cmd(token, target_sessions, transformer)

    def handle_private_cmd(self, cmd: str):
        print("Executing private cmd.")
        cmd = cmd.strip()
        if cmd == "p-session-meta":
            print("Printing all session meta...")
            print(StateManager.inst().get_all_session_meta())
        elif cmd == "p-session-manager-meta":
            print("Printing all session manager meta...")
            print(StateManager.inst()) 
        else:
            print("Unknown private command.")