from abc import ABC, abstractmethod
import asyncio
from dataclasses import dataclass
import time
from typing import Dict, List, Optional, Set, Tuple, Union
from threading import Lock
from iddb.data_struct import SessionResponse
from iddb.logging import logger
from iddb.cmd_router import CmdRouter
from iddb.mi_formatter import MIFormatter
from iddb.response_transformer import NullTransformer, PlainTransformer, ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, ThreadInfoTransformer, TransformerBase
from iddb.state_manager import StateManager, ThreadContext, ThreadStatus
from iddb.utils import dev_print, parse_cmd
from iddb.mtracer import GlobalTracer
import sys
from iddb.utils import ip_int2ip_str

def prepare_ctx_switch_args(registers: Dict[str, int]) -> str:
    arg = ""
    for (reg, val) in registers.items():
        if val is not None and val:
            arg += f"{reg}={val} "
    return arg.strip()

@dataclass
class SingleCommand:
    token: str
    origin_token: str
    command_no_token: str
    # from user's perspective they are only aware of threads but not sessions
    thread_id: Optional[int] = None
    # for internal use
    session_id: Optional[int] = None
    result_transformer: Optional['TransformerBase'] = None

    @property
    def command(self) -> str:
        return f"{self.token}{self.command_no_token}"


class CmdHandler(ABC):
    def __init__(self, router: CmdRouter):
        self.router = router
        self.state_mgr = StateManager().inst()

    @abstractmethod
    def process_command(self, command_instance: SingleCommand):
        logger.debug(f"Received command in base handler: {command_instance}")

        if command_instance.session_id:
            self.router.send_to_session(command_instance.token, command_instance.command,
                                        command_instance.result_transformer, command_instance.session_id)
        elif command_instance.thread_id == -1:
            self.router.broadcast(
                command_instance.token, command_instance.command, command_instance.result_transformer)
        # fallback send to first thread
        elif not command_instance.thread_id:
            self.router.send_to_first(
                command_instance.token, command_instance.command, command_instance.result_transformer)
        else:
            self.router.send_to_thread(command_instance.thread_id, command_instance.token, command_instance.command,
                                       command_instance.result_transformer)


class CmdHandlerBase(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        super().process_command(command_instance)


class BreakInsertCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        super().process_command(command_instance)


class ThreadInfoCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        logger.debug(f"Received command in handler: {command_instance}")
        command_instance.thread_id = -1
        command_instance.result_transformer = ThreadInfoTransformer()
        super().process_command(command_instance)


class ContinueCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        for sid, session in self.state_mgr.sessions.items():
            async with remotebtlock:
                if session.in_custom_context:
                    ctx_switch_args = prepare_ctx_switch_args(session.current_context.ctx)
                    restore_cmd, restore_cmd_token, _ = self.router.prepend_token(
                        f"-switch-context-custom {ctx_switch_args}"
                    )
                    context_switch_result = await self.router.send_to_thread_async(session.current_context.thread_id, restore_cmd_token, f"{restore_cmd_token}{restore_cmd}", transformer=NullTransformer())
                    assert len(context_switch_result) == 1
                    if context_switch_result[0].payload["message"] != "success":
                        return
                    self.state_mgr.sessions[sid].in_custom_context = False

        super().process_command(command_instance)


class InterruptCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        state_manager = StateManager.inst()
        for sid, session in state_manager.sessions.items():
            for status in session.t_status.values():
                if status == ThreadStatus.RUNNING:
                    command_instance.session_id = sid
                    super().process_command(command_instance)
                    break

class ListCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        self.state_mgr.set_current_session(1)
        self.send_to_current_session(command_instance)


class ThreadSelectCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        cmd_split = command_instance.command_no_token.split()
        if len(cmd_split) > 1:
            gtid = int(cmd_split[-1])
            sid, tid = self.router.state_mgr.get_sidtid_by_gtid(gtid)
            command_instance.thread_id = gtid
            command_instance.command_no_token = f"-thread-select {tid}\n"
        super().process_command(command_instance)

remotebtlock = asyncio.Lock()

class RemoteBacktraceHandler(CmdHandler):
    def extract_remote_metadata(self, data):
        caller_meta = data.get('metadata', {}).get('caller_meta', {})
        caller_ctx = data.get('metadata', {}).get('caller_ctx', {})
        pid, ip_int = caller_meta.get('pid'), int(caller_meta.get('ip'))
        return {
            'message': data.get('message'),
            'caller_ctx': caller_ctx,
            # 'pc': caller_meta.get('pc'),
            # 'sp': caller_meta.get('sp'),
            # 'fp': caller_meta.get('fp'),
            # 'lr': caller_meta.get('lr', None),
            'id': f"{ip_int2ip_str(ip_int)}:-{pid}" if 0 <= ip_int <= 0xFFFFFFFF else pid 
        }

    async def process_command(self, command_instance: SingleCommand):
        if not command_instance.thread_id:
            return
        aggreated_bt_result = []
        current_sid, current_tid = self.state_mgr.get_sidtid_by_gtid(
            command_instance.thread_id)
        a_bt_result = await self.router.send_to_session_async(command_instance.token, f"{command_instance.token}-stack-list-frames --thread {current_tid}", session_id=current_sid, transformer=NullTransformer())
        assert (len(a_bt_result) == 1)
        aggreated_bt_result.append(a_bt_result[0].payload)
        for frame in a_bt_result[0].payload['stack']:
            frame['session'] = current_sid
            frame['thread'] = command_instance.thread_id
        try:
            remote_bt_cmd, remote_bt_token, _ = self.router.prepend_token("-get-remote-bt")
            remote_bt_parent_info = await self.router.send_to_thread_async(
                command_instance.thread_id, remote_bt_token, f"{remote_bt_token}{remote_bt_cmd}", NullTransformer()
            )
            assert len(remote_bt_parent_info) == 1
            remote_bt_parent_info = self.extract_remote_metadata(
                remote_bt_parent_info[0].payload
            )
            while remote_bt_parent_info.get("message") == 'success':
                logger.debug(
                    "trying to acquire parent info:-------------------------------------------------")
                parent_session_id = self.state_mgr.get_session_by_tag(
                    remote_bt_parent_info.get("id")
                )
                chosen_id = self.state_mgr.get_gtids_by_sid(parent_session_id)[0]
                async with remotebtlock:
                    if not self.state_mgr.sessions[parent_session_id].in_custom_context:
                        # interrupt_cmd, interrupt_cmd_token, _ = self.router.prepend_token(
                        #     "-exec-interrupt")
                        # self.router.send_to_session(
                        #     interrupt_cmd_token, interrupt_cmd, session_id=parent_session_id)
                        # just try to busy waiting here

                        ## TODO: right now we need DA to send interrupt to 
                        # all process to stop it upon breakpoints hit event
                        # We may need to consider interrupting it in ddb itself.
                        while self.state_mgr.sessions[parent_session_id].t_status[1] != ThreadStatus.STOPPED:
                            await asyncio.sleep(0.1)
                        ctx_switch_arg = prepare_ctx_switch_args(remote_bt_parent_info.get('caller_ctx'))
                        context_switch_cmd, context_switch_token, _ = self.router.prepend_token(
                            f"-switch-context-custom {ctx_switch_arg}"
                        )
                        # default choose the first thread as remote-bt-thread
                        context_switch_result = await self.router.send_to_thread_async(chosen_id, context_switch_token, f"{context_switch_token}{context_switch_cmd}",  transformer=NullTransformer())
                        assert len(context_switch_result) == 1
                        if context_switch_result[0].payload["message"] != "success":
                            return
                        
                        ctx_to_save = { 
                            str(reg): int(val) 
                            for (reg, val) in context_switch_result[0].payload["old_ctx"].items()
                        }
                        # ctx_to_save = {}
                        # for (reg, val) in context_switch_result[0].payload["old_ctx"].items():
                        #     ctx_to_save[str(reg)] = int(val)
                        self.state_mgr.sessions[parent_session_id].current_context = ThreadContext(
                            ctx=ctx_to_save,
                            thread_id=chosen_id
                        )
                        self.state_mgr.sessions[parent_session_id].in_custom_context = True
                chosen_id = self.state_mgr.sessions[parent_session_id].current_context.thread_id
                logger.debug("chosen_id: %d" % chosen_id)
                remote_bt_cmd, remote_bt_token, _ = self.router.prepend_token(
                    f"-get-remote-bt")
                remote_bt_parent_info = await self.router.send_to_thread_async(chosen_id, remote_bt_token, f"{remote_bt_token}{remote_bt_cmd}", transformer=NullTransformer())
                assert len(remote_bt_parent_info) == 1
                list_stack_cmd, list_stack_token, _ = self.router.prepend_token(
                    f"-stack-list-frames")
                bt_result = await self.router.send_to_thread_async(chosen_id, list_stack_token, f"{list_stack_token}{list_stack_cmd}", transformer=NullTransformer())
                assert (len(bt_result) == 1)
                aggreated_bt_result.append(bt_result[0].payload)
                for frame in bt_result[0].payload['stack']:
                    frame['session'] = parent_session_id
                    frame['thread'] = chosen_id
                a_bt_result[0].payload['stack'].extend(
                    bt_result[0].payload['stack'])
                remote_bt_parent_info = self.extract_remote_metadata(
                    remote_bt_parent_info[0].payload
                )
        except Exception as e:
            exc_type, exc_value, exc_traceback = sys.exc_info()
            logger.debug(
                f"Error in remote backtrace: {e}")
        finally:
            print("\n" + f"[ TOOL MI OUTPUT ] \n{(PlainTransformer().format(a_bt_result))}\n")


class ListGroupsCmdHandler(CmdHandlerBase):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        command_instance.result_transformer = ProcessReadableTransformer()
        super().process_command(command_instance)


class CommandProcessor:
    def __init__(self, router: CmdRouter):
        self.alock = asyncio.Lock()
        self.router = router
        self.command_handlers: dict[str, CmdHandler] = {
            "-break-insert": BreakInsertCmdHandler(self.router),
            "-thread-info": ThreadInfoCmdHandler(self.router),
            "-exec-continue": ContinueCmdHandler(self.router),
            "-exec-interrupt": InterruptCmdHandler(self.router),
            "-file-list-lines": ListCmdHandler(self.router),
            "-thread-select": ThreadSelectCmdHandler(self.router),
            "-bt-remote": RemoteBacktraceHandler(self.router),
            "-list-thread-groups": ListGroupsCmdHandler(self.router),
            "-exec-next": CmdHandlerBase(self.router),
        }
        self.base_handler = CmdHandlerBase(self.router)

    def is_ready(self):
        for _, s in self.router.sessions.items():
            if not s.gdb_controller.is_open():
                logger.debug(f"not ready yet")
                return False
        return True

    def register_handler(self, patterns: list[str], handler_class: type[CmdHandler]):
        handler = handler_class(self.router)
        for pattern in patterns:
            self.command_handlers[pattern] = handler

    async def send_command(self, cmd: str):
        while not self.is_ready():
            await asyncio.sleep(0.5)
        # Command parsing and preparation logic
        cmd_no_token, token, origin_token = self.router.prepend_token(cmd)
        if not (cmd_split := cmd_no_token.split()):
            return
        with GlobalTracer().tracer.start_as_current_span("process_command", attributes={"command": f"{token}{cmd_no_token}", "token": token}):
            prefix = cmd_split[0]
            cmd_instance = SingleCommand(
                token=token, origin_token=origin_token, command_no_token=cmd_no_token)
            # Command routing logic
            if len(cmd_split) >= 2 and cmd_split[-1] == "--all":
                cmd_instance.thread_id = -1
                cmd_instance.command_no_token = " ".join(cmd_split[0:-1])
            elif "--thread" in cmd_split:
                thread_index = cmd_split.index("--thread")
                if thread_index < len(cmd_split) - 1:
                    gtid = int(cmd_split[thread_index + 1])
                    sid, tid = self.router.state_mgr.get_sidtid_by_gtid(gtid)
                    cmd_instance.thread_id = gtid
                    cmd_split[thread_index + 1] = str(tid)
                    cmd_instance.command_no_token = " ".join(cmd_split)
            elif curr_thread := self.router.state_mgr.get_current_gthread():
                sid, tid = self.router.state_mgr.get_sidtid_by_gtid(
                    curr_thread)
                cmd_instance.thread_id = curr_thread

            if "--session" in cmd_split: 
                session_index = cmd_split.index("--session")
                if session_index < len(cmd_split) - 1:
                    sid = int(cmd_split[session_index + 1])
                    cmd_instance.session_id = sid
                    cmd_split.pop(session_index + 1)  # Remove the session ID
                    cmd_split.pop(session_index)      # Remove "--session"
                    cmd_instance.command_no_token = " ".join(cmd_split)

            # Command handling

            handler = self.command_handlers.get(prefix)
            GlobalTracer().request_times[token] = time.time_ns()
            # async with self.alock:
            if not handler:
                return await self.base_handler.process_command(cmd_instance)
            return await handler.process_command(cmd_instance)
