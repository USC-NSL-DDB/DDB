import asyncio
from pprint import pprint
from threading import Lock, Thread
from typing import List, Optional, Set, Tuple, Union
from gdb_session import GdbSession
from cmd_tracker import CmdTracker
from counter import TSCounter
from event_loop import EventLoopThread
from response_transformer import ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, ThreadInfoReadableTransformer, ThreadInfoTransformer
from state_manager import StateManager, ThreadStatus

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


''' Routing all commands to the desired gdb sessions
`CmdRouter` will fetch a token from `CmdTokenGenerator` and prepend the token to the cmd. 
`CmdRouter` will partially parse/extract the token and command to ensure it will be resgitered with the `CmdTracker`.
`CmdRouter` also handles the private commands which can be used to print out some internal states

**Key Functions**: `send_cmd(str)`
'''


def extract_remote_parent_data(data):
    try:
        metadata = data.get('metadata', {})
        parent_rip = metadata.get('parentRIP', -1)
        parent_rsp = metadata.get('parentRSP', -1)
        parent_addr = metadata.get('parentAddr', [])
        parent_port = metadata.get('parentPort', -1)
        parent_addr_str = '.'.join(str(octet) for octet in parent_addr[-4:])

        return {
            'parent_rip': parent_rip,
            'parent_rsp': parent_rsp,
            'parent_addr': parent_addr_str,
            'parent_port': parent_port
        }
    except (KeyError, TypeError):
        return {
            'parent_rip': 'N/A',
            'parent_rsp': 'N/A',
            'parent_addr': 'N/A',
            'parent_port': 'N/A'
        }


remoteBt = True


class CmdRouter:
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.sessions = {s.sid: s for s in sessions}
        self.state_mgr = StateManager.inst()
        self.event_loop_thread=EventLoopThread()
        Thread(target=self.event_loop_thread.run,args=()).start()

    def prepend_token(self, cmd: str) -> Tuple[str, str]:
        token = CmdTokenGenerator.get()
        return f"{token}{cmd}", str(token)

    # TODO: handle the case where external command passed in carries a token

    async def send_cmd(self, cmd: str):
        print("sending cmd through the CmdRouter...")

        if len(cmd.strip()) == 0:
            # special case of no meaningful command
            return

        if cmd[0] == ":":
            # handle private command
            self.handle_private_cmd(cmd[1:])
            return

        cmd, _ = self.prepend_token(cmd)
        print("current cmd:", cmd)
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

        if (prefix in ["b", "break", "-break-insert"]):
            self.broadcast(token, cmd)
        elif (prefix in ["bt", "backtrace", "-stack-list-frames"]):
            if not remoteBt:
                self.send_to_current_thread(token, cmd)
            else:
                aggreated_bt_result = []
                bt_result = await self.send_to_current_thread_async(token, cmd)
                assert(len(bt_result) == 1)
                aggreated_bt_result.append(bt_result[0].payload)
                remote_bt_cmd, remote_bt_token = self.prepend_token(
                    f"-get-remote-bt")
                remote_bt_parent_info = await self.send_to_current_thread_async(remote_bt_token, remote_bt_cmd)
                assert len(remote_bt_parent_info) == 1
                remote_bt_parent_info=extract_remote_parent_data(remote_bt_parent_info[0].payload)
                while remote_bt_parent_info.get("parent_rip") != '-1':
                    print("trying to acquire parent info:-------------------------------------------------")
                    parent_session_id=self.state_mgr.get_session_by_tag(remote_bt_parent_info.get("parent_addr"))
                    interrupt_cmd,interrupt_cmd_token=self.prepend_token("-exec-interrupt")
                    self.send_to_session(interrupt_cmd_token,interrupt_cmd,session_id=parent_session_id)
                    # just try to busy waiting here
                    while self.state_mgr.sessions[parent_session_id].t_status[1]!=ThreadStatus.STOPPED:
                        pass
                    remote_bt_cmd, remote_bt_token = self.prepend_token(
                        f"-get-remote-bt-in-context {remote_bt_parent_info.get('parent_rip')} {remote_bt_parent_info.get('parent_rsp')}")
                    remote_bt_parent_info=await self.send_to_session_async(remote_bt_token, remote_bt_cmd, session_id=parent_session_id)
                    assert len(remote_bt_parent_info) == 1
                    remote_bt_parent_info=remote_bt_parent_info[0].payload
                    parent_stack_info=remote_bt_parent_info.get("stack")
                    aggreated_bt_result.append(parent_stack_info)
                    remote_bt_parent_info=extract_remote_parent_data(remote_bt_parent_info)
                    print("remote_bt_parent_info from in context",remote_bt_parent_info)
                print("[special header]")
                print(aggreated_bt_result)
                        

        elif (prefix in ["run", "r", "-exec-run"]):
            self.broadcast(token, cmd)
        elif (prefix in ["list"]):
            # self.send_to_first(cmd)
            self.state_mgr.set_current_session(1)
            self.send_to_current_session(token, cmd)
        elif (prefix in ["c", "continue", "-exec-continue"]):
            subcmd = cmd_no_token.split()[1] if len(
                cmd_no_token.split()) >= 2 else None
            if subcmd:
                if subcmd == "--all":
                    self.broadcast(token, cmd)
            else:
                self.send_to_current_thread(token, cmd)
            # self.send_to_current_session(token, cmd)
        elif (prefix in ["-thread-select"]):
            if len(cmd_no_token.split()) < 2:
                print("Usage: -thread-select #gtid")
                return
            self.state_mgr.set_current_gthread(int(cmd_no_token.split()[1]))
        elif (prefix in ["-thread-info"]):
            self.broadcast(token, cmd, ThreadInfoTransformer())
        elif (prefix in ["-list-thread-groups"]):
            self.broadcast(token, cmd, ProcessInfoTransformer())
        elif (prefix in ["info"]):
            subcmd = cmd_no_token.split()[1]
            if subcmd == "threads" or subcmd == "thread":
                self.broadcast(token, f"{token}-thread-info",
                               ThreadInfoReadableTransformer())
            if subcmd == "inferiors" or subcmd == "inferior":
                self.broadcast(
                    token, f"{token}-list-thread-groups", ProcessReadableTransformer())
        else:
            subcmd = cmd_no_token.split()[1] if len(
                cmd_no_token.split()) >= 2 else None
            if subcmd:
                if subcmd == "--all":
                    self.broadcast(token, cmd)
            else:
                self.send_to_current_thread(token, cmd)
            # self.send_to_current_session(token, cmd)
            # self.broadcast(cmd)

        # if (cmd.strip() in [ ] )
        # for s in self.sessions:
        #     s.write(cmd)

    def send_to_thread(self, gtid: int, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        sid, tid = self.state_mgr.get_sidtid_by_gtid(gtid)
        self.register_cmd(token, sid, transformer)
        # [ s.write(cmd) for s in self.sessions if s.sid == curr_thread ]
        self.sessions[sid].write("-thread-select " + str(tid) + "\n" + cmd)

    # def select_

    def send_to_current_thread(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_thread = self.state_mgr.get_current_gthread()
        if not curr_thread:
            print("use -thread-select #gtid to select the thread.")
            return
        self.send_to_thread(curr_thread, token, cmd, transformer)

    async def send_to_current_thread_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_thread = self.state_mgr.get_current_gthread()
        if not curr_thread:
            print("use -thread-select #gtid to select the thread.")
            return
        self.send_to_thread(curr_thread, token, cmd, transformer)
        future = CmdTracker.inst().waiting_cmds[token]
        print("current future", future, id(future))
        result = await future
        return result

    def send_to_current_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            print("use session #sno to select session.")
            return

        self.register_cmd(token, curr_session, transformer)
        [s.write(cmd)
         for _, s in self.sessions.items() if s.sid == curr_session]

    def broadcast(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd_for_all(token, transformer)
        for _, s in self.sessions.items():
            s.write(cmd)

    # def send_to_random_one(self, cmd: str):

    def send_to_first(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd(token, self.sessions[1].sid, transformer)
        self.sessions[1].write(cmd)

    def send_to_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        if session_id == -1:
            raise Exception("session is None")
        print("current async session:",self.sessions[session_id])
        self.register_cmd(token, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)
    async def send_to_session_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        if session_id == -1:
            raise Exception("session is None")
        print("current async session:",self.sessions[session_id])
        self.register_cmd(token, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)
        future = CmdTracker.inst().waiting_cmds[token]
        print("current future", future, id(future))
        result = await future
        return result
    # Some help functions for registering cmds

    def register_cmd_for_all(self, token: Optional[str], transformer: Optional[ResponseTransformer] = None):
        target_s_ids = set()
        for sid in self.sessions:
            target_s_ids.add(sid)
        self.register_cmd(token, target_s_ids, transformer)

    def register_cmd(self, token: Optional[str], target_sessions: Union[int, Set[int]], transformer: Optional[ResponseTransformer] = None):
        print("registering cmd...")
        print("token:", token)
        print("target_sessions:", target_sessions)
        if token:
            if isinstance(target_sessions, int):
                target_sessions = {target_sessions}

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
