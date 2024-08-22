import re
from threading import Lock
from typing import List, Optional, Set, Tuple, Union
from ddb.gdb_session import GdbSession
from ddb.cmd_tracker import CmdTracker
from ddb.state_manager import StateManager, ThreadStatus
from ddb.utils import CmdTokenGenerator, dev_print, parse_cmd
from ddb.response_transformer import ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, ThreadInfoReadableTransformer, ThreadInfoTransformer, ThreadSelectTransformer, TransformerBase
from ddb.logging import logger

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
        if token:
            self.register_cmd(
                token, cmd, self.sessions[session_id].sid, transformer)
        self.sessions[session_id].write(cmd)

    async def send_to_session_async(self, token: Optional[str], cmd: str, transformer: Optional[ResponseTransformer] = None, session_id: Optional[int] = -1):
        if session_id == -1:
            raise Exception("session is None")
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
