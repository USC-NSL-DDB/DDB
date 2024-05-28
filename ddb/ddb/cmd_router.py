from threading import Lock, Thread
from typing import List, Optional, Set, Tuple, Union
from ddb.gdb_session import GdbSession
from ddb.cmd_tracker import CmdTracker
from ddb.counter import TSCounter
from ddb.event_loop import EventLoopThread
from ddb.state_manager import StateManager, ThreadStatus
from ddb.utils import CmdTokenGenerator, dev_print, parse_cmd
from ddb.response_transformer import BacktraceReadableTransformer, ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, StackListFramesTransformer, ThreadInfoReadableTransformer, ThreadInfoTransformer, ThreadSelectTransformer

''' Routing all commands to the desired gdb sessions
`CmdRouter` will fetch a token from `CmdTokenGenerator` and prepend the token to the cmd. 
`CmdRouter` will partially parse/extract the token and command to ensure it will be resgitered with the `CmdTracker`.
`CmdRouter` also handles the private commands which can be used to dev_print out some internal states

**Key Functions**: `send_cmd(str)`
'''

FORCE_INTERRUPT_ON_COMMADN = True

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
import re
def get_token_and_command(command):
    pattern = r'^(\d+)-.+$'
    match = re.match(pattern, command)
    if match:
        token = match.group(1)
        end_pos = match.end(1)
        command = command[end_pos:]
        return token, command
    else:
        return None, None

class CmdRouter:
    """ 
    Routing all commands to the desired gdb sessions.

    - `CmdRouter` will fetch a token from `CmdTokenGenerator` and prepend the token to the cmd.   
    - `CmdRouter` will partially parse/extract the token and command to ensure it will be resgitered with the `CmdTracker`.  
    - `CmdRouter` also handles the private commands which can be used to dev_print out some internal states.  

    **Key Functions**: `send_cmd(str)`
    """
    # Should start sessions in this object?
    def __init__(self, sessions: List[GdbSession]) -> None:
        self.lock = Lock()
        self.sessions = {s.sid: s for s in sessions}
        self.state_mgr = StateManager.inst()
        self.event_loop_thread=EventLoopThread()
        Thread(target=self.event_loop_thread.run,args=()).start()

    def add_session(self, session: GdbSession):
        with self.lock:
            self.sessions[session.sid] = session

    def prepend_token(self, cmd: str) -> Tuple[str, str]:
        token,command=get_token_and_command(cmd)
        if not token:
            token = CmdTokenGenerator.get()
            command=cmd
        token=CmdTracker.inst().dedupToken(token)
        return f"{token}{command}", str(token)

    # TODO: handle the case where external command passed in carries a token
    async def send_cmd(self, cmd: str):
        dev_print("sending cmd through the CmdRouter...")

        if len(cmd.strip()) == 0:
            # special case of no meaningful command
            return

        if cmd[0] == ":":
            # handle private command
            self.handle_private_cmd(cmd[1:])
            return
        cmd, _ = self.prepend_token(cmd)
        print("current cmd:", cmd)
        token, cmd_no_token, prefix, cmd = parse_cmd(cmd) 
        cmd = f"{token}{cmd_no_token}\n"
        
        if (prefix in ["b", "break", "-break-insert"]):
            self.broadcast(token, cmd)
        elif (prefix in ["bt-remote"]):
            aggreated_bt_result = []
            bt_result = await self.send_to_current_thread_async(token, f"{token}-stack-list-frames")
            assert(len(bt_result) == 1)
            aggreated_bt_result.append(bt_result[0].payload)
            remote_bt_cmd, remote_bt_token = self.prepend_token(
                f"-get-remote-bt")
            remote_bt_parent_info = await self.send_to_current_thread_async(remote_bt_token, remote_bt_cmd)
            assert len(remote_bt_parent_info) == 1
            remote_bt_parent_info=extract_remote_parent_data(remote_bt_parent_info[0].payload)
            while remote_bt_parent_info.get("parent_rip") != '-1':
                dev_print("trying to acquire parent info:-------------------------------------------------")
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
                dev_print("remote_bt_parent_info from in context",remote_bt_parent_info)
            print("[special header]")
            print(aggreated_bt_result)
        elif (prefix in ["run", "r", "-exec-run"]):
            self.broadcast(token, cmd)
        elif (prefix in ["list"]):
            # self.send_to_first(cmd)
            self.state_mgr.set_current_session(1)
            self.send_to_current_session(token, cmd)
        elif (prefix in ["-thread-select"]):
            if len(cmd_no_token.split()) < 2:
                print("Usage: -thread-select #gtid")
                return
            gtid=int(cmd_no_token.split()[1])
            self.state_mgr.set_current_gthread(gtid)
            session_id,thread_id=self.state_mgr.get_sidtid_by_gtid(gtid)
            new_cmd=cmd.split()[0]+" "+str(thread_id)
            self.send_to_session(token,new_cmd,ThreadSelectTransformer(gtid),session_id)
        elif (prefix in ["-thread-info"]):
            self.broadcast(token, cmd, ThreadInfoTransformer())
        elif (prefix in ["-list-thread-groups"]):
            self.broadcast(token, cmd, ProcessInfoTransformer())
        elif (prefix in [ "info" ]):
            subcmd = cmd_no_token.split()[1]
            if subcmd == "threads" or subcmd == "thread":
                self.broadcast(
                    token, f"{token}-thread-info", ThreadInfoReadableTransformer()
                )
            if subcmd == "inferiors" or subcmd == "inferior":
                self.broadcast(
                    token, f"{token}-list-thread-groups", ProcessReadableTransformer())
        else:
            cmd_split = cmd.split()
            if len(cmd_split) >= 2 and cmd_split[-1] == "--all":
                self.broadcast(token, " ".join(cmd_split[0:-1]))
            elif "--thread" in cmd_split:
                thread_index = cmd_split.index("--thread")
                if thread_index < len(cmd_split) - 1:
                    gtid = int(cmd_split[thread_index + 1])
                    _, tid = self.state_mgr.get_sidtid_by_gtid(gtid)
                    cmd_split[thread_index + 1] = str(tid)
                    self.send_to_thread(gtid, token, " ".join(cmd_split))
            else:
                self.send_to_current_thread(token, cmd)
    
    def prepare_force_interrupt_command(self, cmd: str, resume: bool = True) -> str:
        cmd_back = cmd
        if not cmd_back.endswith("\n"):
            cmd_back = f"{cmd_back}\n"
        if FORCE_INTERRUPT_ON_COMMADN:
            cmd_back = f"-exec-interrupt\n {cmd_back}"
            if resume:
                # using `-exec-continue --all` with `--all` to ensure 
                # it works correctly when non-stop mode is enabled
                cmd_back = f"{cmd_back} -exec-continue --all\n"
        return cmd_back

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

    async def send_to_current_thread_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_thread = self.state_mgr.get_current_gthread()
        if not curr_thread:
            print("use -thread-select #gtid to select the thread.")
            return
        self.send_to_thread(curr_thread, token, cmd, transformer)
        future = CmdTracker.inst().get_cmdmeta(token)
        dev_print("current future", future, id(future))
        result = await future
        return result

    def send_to_current_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            dev_print("use session #sno to select session.")
            return

        self.register_cmd(token, curr_session, transformer)
        [s.write(cmd)
        for _, s in self.sessions.items() if s.sid == curr_session]

    def broadcast(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd_for_all(token, transformer)
        for _, s in self.sessions.items():
            cmd_to_send = cmd
            if FORCE_INTERRUPT_ON_COMMADN:
                # We only force interrupt if the thread is running
                s_meta = StateManager.inst().get_session_meta(s.sid)
                dev_print(f"Broadcast - Session {s.sid} meta: \n{s_meta}")
                # We assume in all-stop mode, so only check the first thread status. 
                # Assumption is all threads are at the same status.
                cond = (s_meta and len(s_meta.t_status) > 0) and s_meta.t_status[1] == ThreadStatus.RUNNING
                dev_print(f"cond: {cond}")
                if cond:
                    cmd_to_send = self.prepare_force_interrupt_command(cmd_to_send, resume=True)
            # dev_print("Comand on broadcast to :\n", cmd)
            s.write(cmd_to_send)

    def send_to_first(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd(token, self.sessions[1].sid, transformer)
        self.sessions[1].write(cmd)

    def send_to_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        assert(session_id>=0 and session_id<=len(self.state_mgr.sessions)),"invalid session id for `send_to_session`"
        dev_print("current async session:",self.sessions[session_id])
        self.register_cmd(token, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)

    async def send_to_session_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        if session_id == -1:
            raise Exception("session is None")
        dev_print("current async session:",self.sessions[session_id])
        self.register_cmd(token, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)
        future = CmdTracker.inst().get_cmdmeta(token)
        dev_print("current future", future, id(future))
        result = await future
        return result
    # Some help functions for registering cmds

    def register_cmd_for_all(self, token: Optional[str], transformer: Optional[ResponseTransformer] = None):
        target_s_ids = set()
        for sid in self.sessions:
            target_s_ids.add(sid)
        self.register_cmd(token, target_s_ids, transformer)

    def register_cmd(self, token: Optional[str], target_sessions: Union[int, Set[int]], transformer: Optional[ResponseTransformer] = None):
        dev_print("registering cmd...")
        dev_print("token:", token)
        dev_print("target_sessions:", target_sessions)
        if token:
            if isinstance(target_sessions, int):
                target_sessions = {target_sessions}

            if not isinstance(target_sessions, Set):
                raise Exception("wrong argument")

            CmdTracker.inst().create_cmd(token, target_sessions, transformer)

    def handle_private_cmd(self, cmd: str):
        dev_print("Executing private cmd.")
        cmd = cmd.strip()
        if cmd == "p-session-meta":
            dev_print("Printing all session meta...")
            dev_print(StateManager.inst().get_all_session_meta())
        elif cmd == "p-session-manager-meta":
            dev_print("Printing all session manager meta...")
            dev_print(StateManager.inst())
        else:
            dev_print("Unknown private command.")
