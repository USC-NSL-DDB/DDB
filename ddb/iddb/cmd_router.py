import re
from threading import Lock
from typing import List, Optional, Set, Tuple, Union
from iddb.config import GlobalConfig
from iddb.data_struct import SessionResponse, TargetFramework
from iddb.gdb_session import GdbSession
from iddb.cmd_tracker import CmdTracker
from iddb.state_manager import StateManager, ThreadStatus
from iddb.utils import CmdTokenGenerator, dev_print, parse_cmd, ip_int2ip_str
from iddb.response_transformer import ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, StackListFramesTransformer,ThreadInfoReadableTransformer, ThreadInfoTransformer, ThreadSelectTransformer, TransformerBase
from iddb.logging import logger

''' Routing all commands to the desired gdb sessions
`CmdRouter` will fetch a token from `CmdTokenGenerator` and prepend the token to the cmd. 
`CmdRouter` will partially parse/extract the token and command to ensure it will be resgitered with the `CmdTracker`.
`CmdRouter` also handles the private commands which can be used to dev_print out some internal states

**Key Functions**: `send_cmd(str)`
'''

# Temporarily disable this as it don't work as expected.
# Problem: https://github.com/USC-NSL/distributed-debugger/issues/61
# Current solution: https://github.com/USC-NSL/distributed-debugger/issues/62
FORCE_INTERRUPT_ON_COMMADN = False

def extract_remote_parent_data(data):
    metadata = data.get('metadata', {})
    parent_rip = metadata.get('parentRIP', '-1')
    parent_rsp = metadata.get('parentRSP', '-1')
    parent_addr = metadata.get('parentAddr', [])
    parent_port = metadata.get('parentPort', '-1')
    parent_addr_str = '.'.join(str(octet) for octet in parent_addr[-4:])

    return {
        'parent_rip': parent_rip,
        'parent_rsp': parent_rsp,
        'parent_addr': parent_addr_str,
        'parent_port': parent_port
    }

def nu_extract_remote_parent_data(data) -> Optional[dict]:
    metadata = data.get('bt_meta', None)
    if not metadata:
        return None

    caller_meta = metadata.get('caller_meta', {})
    parent_rip = int(caller_meta.get('rip', 0))
    parent_rsp = int(caller_meta.get('rsp', 0))
    parent_rbp = int(caller_meta.get('rbp', 0))
    parent_pid = int(caller_meta.get('pid', 0))

    parent_addr = metadata.get('remote_addr', {}).get("ip", 0)
    parent_addr = ip_int2ip_str(int(parent_addr))
    # parent_port = metadata.get('parentPort', '-1')
    # parent_addr_str = '.'.join(str(octet) for octet in parent_addr[-4:])

    return {
        'rip': parent_rip,
        'rsp': parent_rsp,
        'rbp': parent_rbp,
        'pid': parent_pid,
        'parent_addr': parent_addr
    }

def nu_extract_stack(data) -> Optional[list]:
    return data.get('stack', None)

def nu_concat_stack(stack: List[dict], bt_data: dict) -> List[dict]:
    bt_stack: List[dict] = nu_extract_stack(bt_data)
    for frame in bt_stack:
        loc_frame = frame.copy()
        loc_frame["level"] = f"{len(stack)}"
        stack.append(loc_frame)
    return stack


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
    def add_session(self, session: GdbSession):
        with self.lock:
            self.sessions[session.sid] = session

    def prepend_token(self, cmd: str) -> Tuple[str, str, str]:
        origin_token, command = get_token_and_command(cmd)
        if not origin_token:
            token = CmdTokenGenerator.get()
            command = cmd
        else:
            token = origin_token
        token = CmdTracker.inst().dedupToken(token)
        return str(command), str(token), str(origin_token)

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
        elif (prefix in ["-bt-remote"]):
            logger.debug("-bt-remote")
            if GlobalConfig.get().framework == TargetFramework.NU:
                logger.debug("execute -bt-remote")
                stack = []
                bt_cmd, bt_token = self.prepend_token("-stack-list-distri-frames")
                bt_result = await self.send_to_current_thread_async(bt_token, bt_cmd)
                stack = nu_concat_stack(stack, bt_result[0].payload)
                # logger.debug(f"bt_result: {bt_result[0].payload}")
                parent_meta = nu_extract_remote_parent_data(bt_result[0].payload)
                # logger.debug(f"parent meta: {parent_meta}")
                while parent_meta:
                    rip = parent_meta["rip"]
                    rsp = parent_meta["rsp"]
                    rbp = parent_meta["rbp"]
                    pid = parent_meta["pid"]
                    addr = parent_meta["parent_addr"]
                    target_tag = f"{addr}:-{pid}"
                    # right now it only works for auto service discovery...
                    parent_sid = self.state_mgr.get_session_by_tag(target_tag)
                    # interrupt_cmd,interrupt_cmd_token=self.prepend_token("-exec-interrupt")
                    self.send_to_session(None, "-exec-interrupt",session_id=parent_sid)
                    # just try to busy waiting here
                    while self.state_mgr.sessions[parent_sid].t_status[1]!=ThreadStatus.STOPPED:
                        pass
                    cmd_token, token = self.prepend_token(f"-stack-list-distri-frames-ctx {rip} {rsp} {rbp}")
                    parent_bt_info = await self.send_to_session_async(token, cmd_token, session_id=parent_sid)
                    payload = parent_bt_info[0].payload
                    stack = nu_concat_stack(stack, payload)
                    parent_meta = nu_extract_remote_parent_data(payload)
                curr_s_id = self.state_mgr.get_current_session()
                curr_s_meta_str = self.sessions[curr_s_id].get_meta_str()
                sr = SessionResponse(curr_s_id, curr_s_meta_str, response={
                    "type": "result",
                    "message": "done",
                    "stream": "stdout",
                    "payload": {
                        "stack": stack
                    }
                })
                ResponseTransformer.output(sr, StackListFramesTransformer())
            else:
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
        elif (prefix in ["-gdb-exit", "exit"]):
            self.broadcast(token, cmd)
        else:
            token = origin_token
        token = CmdTracker.inst().dedupToken(token)
        return str(command), str(token), str(origin_token)
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
        self.state_mgr.set_current_session(sid)
        self.register_cmd(token, cmd, sid, transformer)
        # [ s.write(cmd) for s in self.sessions if s.sid == curr_thread ]
        self.sessions[sid].write(
            "-thread-select " + str(tid) + "\n" + 
                                 cmd)
    async def send_to_thread_async(self, gtid: int, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.send_to_thread(gtid, token, cmd, transformer)
        future = CmdTracker.inst().get_cmdmeta(token)
        result = await future
        return result

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
        result = await future
        return result

    def send_to_current_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        curr_session = self.state_mgr.get_current_session()
        if not curr_session:
            return

        self.register_cmd(token, cmd, curr_session, transformer)
        [s.write(cmd)
        for _, s in self.sessions.items() if s.sid == curr_session]

    def broadcast(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd_for_all(token, cmd, transformer)
        for _, s in self.sessions.items():
            cmd_to_send = cmd
            if FORCE_INTERRUPT_ON_COMMADN:
                # We only force interrupt if the thread is running
                s_meta = StateManager.inst().get_session_meta(s.sid)
                logger.debug(f"Broadcast - Session {s.sid} meta: \n{s_meta}")
                # We assume in all-stop mode, so only check the first thread status. 
                # Assumption is all threads are at the same status.
                cond = (s_meta and len(s_meta.t_status) > 0) and s_meta.t_status[1] == ThreadStatus.RUNNING
                if cond:
                    cmd_to_send = self.prepare_force_interrupt_command(cmd_to_send, resume=True)
            s.write(cmd_to_send)

    def send_to_first(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None):
        self.register_cmd(token, cmd, self.sessions[1].sid, transformer)
        self.sessions[1].write(cmd)

    def send_to_session(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        assert(session_id>=0 and session_id<=len(self.state_mgr.sessions)),"invalid session id for `send_to_session`"
        if session_id:
            self.state_mgr.set_current_session(session_id)
        # dev_print("current async session:",self.sessions[session_id])
        if token:
            self.register_cmd(
                token, cmd, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)

    async def send_to_session_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        if session_id == -1:
            raise Exception("session is None")
        if session_id:
            self.state_mgr.set_current_session(session_id)
        dev_print("current async session:",self.sessions[session_id])
        self.register_cmd(
            token, cmd, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)
        future = CmdTracker.inst().get_cmdmeta(token)
        result = await future
        return result
    # Some help functions for registering cmds

    def register_cmd_for_all(self, token: Optional[str], command: Optional[str], transformer: Optional[ResponseTransformer] = None):
        target_s_ids = set()
        for sid in self.sessions:
            target_s_ids.add(sid)
        self.register_cmd(token, command, target_s_ids, transformer)

    def register_cmd(self, token: Optional[str], command: Optional[str], target_sessions: Union[int, Set[int]], transformer: Optional[ResponseTransformer] = None):
        if token:
            if isinstance(target_sessions, int):
                target_sessions = {target_sessions}

            if not isinstance(target_sessions, Set):
                raise Exception("wrong argument")

            CmdTracker.inst().create_cmd(token, command, target_sessions, transformer)

    def handle_private_cmd(self, cmd: str):
        logger.debug("Executing private cmd.")
        cmd = cmd.strip()
        if cmd == "p-session-meta":
            logger.debug("Printing all session meta...")
            logger.info(StateManager.inst().get_all_session_meta())
        elif cmd == "p-session-manager-meta":
            logger.debug("Printing all session manager meta...")
            logger.info(StateManager.inst())
        elif "s-cmd" in cmd:
            cmd = cmd.split()
            if len(cmd) < 3:
                logger.info("Usage: s-cmd <session_id> <cmd>")
                return
            session_id = int(cmd[1])
            cmd = " ".join(cmd[2:])
            self.send_to_session(None, cmd, session_id=session_id)
        else:
            logger.debug("Unknown private command.")
