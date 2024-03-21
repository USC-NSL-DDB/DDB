from threading import Lock
from typing import List, Optional, Set, Union
from gdb_session import GdbSession
from cmd_tracker import CmdTracker
from counter import TSCounter
from response_transformer import BacktraceReadableTransformer, ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, StackListFramesTransformer, ThreadInfoReadableTransformer, ThreadInfoTransformer
from state_manager import StateManager
from utils import parse_cmd

# A simple wrapper around counter in case any customization later
''' Generate a global unique/incremental token for every cmd it sends
'''
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
    """ 
    Routing all commands to the desired gdb sessions.

    - `CmdRouter` will fetch a token from `CmdTokenGenerator` and prepend the token to the cmd.   
    - `CmdRouter` will partially parse/extract the token and command to ensure it will be resgitered with the `CmdTracker`.  
    - `CmdRouter` also handles the private commands which can be used to print out some internal states.  

    **Key Functions**: `send_cmd(str)`
    """
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = { s.sid: s for s in sessions }
        self.state_mgr = StateManager.inst()

    def prepend_token(self, cmd: str) -> str:
        token = CmdTokenGenerator.get()
        return f"{token}{cmd}"

    # TODO: handle the case where external command passed in carries a token
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
        token, cmd_no_token, prefix, cmd = parse_cmd(cmd) 
        # if token:
        #     CmdTracker.inst().create_cmd(token)
        cmd = f"{cmd}\n"
        
        if (prefix in [ "b", "break", "-break-insert" ]):
            self.broadcast(token, cmd)
        elif (prefix in [ "run", "r", "-exec-run" ]):
            self.broadcast(token, cmd)
        elif (prefix in [ "list" ]):
            # self.send_to_first(cmd)
            self.state_mgr.set_current_session(1)
            self.send_to_current_session(token, cmd)
        elif (prefix in [ "c", "continue", "-exec-continue" ]):
            subcmd = cmd_no_token.split()[1] if len(cmd_no_token.split()) >= 2 else None
            if subcmd:
                if subcmd == "--all":
                    self.broadcast(token, cmd)
            else:
                self.send_to_current_thread(token, cmd)
            # self.send_to_current_session(token, cmd)
        elif (prefix in [ "-thread-select"]):
            if len(cmd_no_token.split()) < 2:
                print("Usage: -thread-select #gtid")
                return
            self.state_mgr.set_current_gthread(int(cmd_no_token.split()[1]))
        elif (prefix in [ "-thread-info" ]):
            self.broadcast(token, cmd, ThreadInfoTransformer())
        elif (prefix in [ "-list-thread-groups" ]):
            self.broadcast(token, cmd, ProcessInfoTransformer())
        elif (prefix in [ "-stack-list-frames" ]):
            self.send_to_current_thread(token, cmd, StackListFramesTransformer())
        elif (prefix in [ "bt", "backtrace", "where" ]):
            self.send_to_current_thread(token, f"{token}-stack-list-frames", BacktraceReadableTransformer())
        elif (prefix in [ "info" ]):
            subcmd = cmd_no_token.split()[1]
            if subcmd == "threads" or subcmd == "thread":
                self.broadcast(token, f"{token}-thread-info", ThreadInfoReadableTransformer())
            if subcmd == "inferiors" or subcmd == "inferior":
                self.broadcast(token, f"{token}-list-thread-groups", ProcessReadableTransformer())
        else:
            self.send_to_current_thread(token, cmd)
            # self.send_to_current_session(token, cmd)
            # self.broadcast(cmd)
        
    def send_to_thread(self, gtid: int, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        sid, tid = self.state_mgr.get_sidtid_by_gtid(gtid)
        self.register_cmd(token, sid, transformer)
        # [ s.write(cmd) for s in self.sessions if s.sid == curr_thread ]
        self.sessions[sid].write("-thread-select " + str(tid) + "\n" + cmd)

    def send_to_current_thread(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_thread = self.state_mgr.get_current_gthread()
        if not curr_thread:
            print("use -thread-select #gtid to select the thread.")
            return
        self.send_to_thread(curr_thread, token, cmd, transformer)

    def send_to_current_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            print("use session #sno to select session.")
            return

        self.register_cmd(token, curr_session, transformer)
        [ s.write(cmd) for _, s  in self.sessions.items() if s.sid == curr_session ]

    def broadcast(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd_for_all(token, transformer)
        for _, s in self.sessions.items():
            s.write(cmd)

    def send_to_first(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd(token, self.sessions[1].sid, transformer) 
        self.sessions[1].write(cmd)
    
    ### Some help functions for registering cmds
    def register_cmd_for_all(self, token: Optional[str], transformer: Optional[ResponseTransformer] = None):
        target_s_ids = set()
        for sid in self.sessions:
            target_s_ids.add(sid)
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
