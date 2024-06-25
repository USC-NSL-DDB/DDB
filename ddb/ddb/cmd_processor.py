from abc import ABC, abstractmethod
import asyncio
from dataclasses import dataclass
from typing import List, Optional, Set, Tuple, Union
from threading import Lock

from ddb.cmd_router import CmdRouter
from ddb.response_transformer import NullTransformer, ProcessInfoTransformer, ProcessReadableTransformer, ResponseTransformer, ThreadInfoTransformer, TransformerBase
from ddb.state_manager import StateManager, ThreadStatus
from ddb.utils import dev_print, parse_cmd


@dataclass
class SingleCommand:
    token: str
    command_no_token: str
    # from user's perspective they are only aware of threads but not sessions
    thread_id: Optional[int] = None
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
        if command_instance.thread_id == -1:
            self.router.broadcast(
                command_instance.token, command_instance.command, command_instance.result_transformer)
        # fallback send to first thread
        elif not command_instance.thread_id:
            self.router.send_to_first(command_instance.token,command_instance.command,command_instance.result_transformer)
        else:
            self.router.send_to_thread(command_instance.token, command_instance.command,
                                        command_instance.result_transformer, command_instance.session_id)


class CmdHandlerBase(CmdHandler):
    def process_command(self, command_instance: SingleCommand):
        super().process_command(command_instance)


class BreakInsertCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        super().process_command(command_instance)


class ThreadInfoCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        command_instance.result_transformer=ThreadInfoTransformer()
        super().process_command(command_instance)


class ContinueCmdHandler(CmdHandler):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        super().process_command(command_instance)


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
            command_instance.command = f"{command_instance.token}-thread-select {tid}\n"
        super().process_command(command_instance)
class RemoteBacktraceHandler(CmdHandler):
    def extract_remote_parent_data(self,data):
        metadata = data.get('metadata', {})
        parent_addr = metadata.get('parentAddr', [])
        
        return {
            'parent_rip': metadata.get('parentRIP', '-1'),
            'parent_rsp': metadata.get('parentRSP', '-1'),
            'parent_addr': '.'.join(map(str, parent_addr[-4:])) if parent_addr else '-1',
            'parent_port': metadata.get('parentPort', '-1')
        }

    async def process_command(self, command_instance: SingleCommand):
        if not command_instance.thread_id:
            return
        aggreated_bt_result = []
        bt_result = await self.router.send_to_current_thread_async(command_instance.token, f"{command_instance.token}-stack-list-frames",NullTransformer())
        assert(len(bt_result) == 1)
        aggreated_bt_result.append(bt_result[0].payload)
        remote_bt_cmd, remote_bt_token = self.router.prepend_token(
            f"-get-remote-bt")
        remote_bt_parent_info = await self.router.send_to_current_thread_async(remote_bt_token, f"{remote_bt_token}{remote_bt_cmd}",NullTransformer())
        assert len(remote_bt_parent_info) == 1
        remote_bt_parent_info=self.extract_remote_parent_data(remote_bt_parent_info[0].payload)
        while remote_bt_parent_info.get("parent_rip") != '-1':
            dev_print("trying to acquire parent info:-------------------------------------------------")
            parent_session_id=self.state_mgr.get_session_by_tag(remote_bt_parent_info.get("parent_addr"))
            interrupt_cmd,interrupt_cmd_token=self.router.prepend_token("-exec-interrupt")
            self.router.send_to_session(interrupt_cmd_token,interrupt_cmd,session_id=parent_session_id)
            # just try to busy waiting here
            while self.state_mgr.sessions[parent_session_id].t_status[1]!=ThreadStatus.STOPPED:
                pass
            remote_bt_cmd, remote_bt_token = self.router.prepend_token(
                f"-get-remote-bt-in-context {remote_bt_parent_info.get('parent_rip')} {remote_bt_parent_info.get('parent_rsp')}")
            remote_bt_parent_info=await self.router.send_to_session_async(remote_bt_token, f"{remote_bt_token}{remote_bt_cmd}", session_id=parent_session_id,transformer=NullTransformer())
            assert len(remote_bt_parent_info) == 1
            remote_bt_parent_info=remote_bt_parent_info[0].payload
            parent_stack_info=remote_bt_parent_info.get("stack")
            aggreated_bt_result.append(parent_stack_info)
            remote_bt_parent_info=self.extract_remote_parent_data(remote_bt_parent_info)
            dev_print("remote_bt_parent_info from in context",remote_bt_parent_info)
        print("[special header]")
        print(aggreated_bt_result)
class ListGroupsCmdHandler(CmdHandlerBase):
    async def process_command(self, command_instance: SingleCommand):
        command_instance.thread_id = -1
        command_instance.result_transformer=ProcessReadableTransformer()
        super().process_command(command_instance)

class CommandProcessor:
    def __init__(self, router: CmdRouter):
        self.router = router
        self.command_handlers: dict[str, CmdHandler] ={
            "-break-insert":BreakInsertCmdHandler(self.router),
            "-thread-info":ThreadInfoCmdHandler(self.router),
            "-exec-continue":ContinueCmdHandler(self.router),
            "-file-list-lines":ListCmdHandler(self.router),
            "-thread-select":ThreadSelectCmdHandler(self.router),
            "-bt-remote":RemoteBacktraceHandler(self.router),
            "-list-thread-groups":ListGroupsCmdHandler(self.router)
        }
        self.base_handler=CmdHandlerBase(self.router)

    def register_handler(self, patterns: list[str], handler_class: type[CmdHandler]):
        handler = handler_class(self.router)
        for pattern in patterns:
            self.command_handlers[pattern] = handler

    async def send_command(self, cmd: str):
        # Command parsing and preparation logic
        cmd_no_token, token = self.router.prepend_token(cmd)
        if not (cmd_split := cmd_no_token.split()): return
        prefix = cmd_split[0]
        cmd_instance = SingleCommand(
            token=token, command_no_token=cmd_no_token)
        # Command routing logic
        if len(cmd_split) >= 2 and cmd_split[-1] == "--all":
            cmd_instance.thread_id = -1
            cmd_instance.command_no_token=" ".join(cmd_split[0:-1])
        elif "--thread" in cmd_split:
            thread_index = cmd_split.index("--thread")
            if thread_index < len(cmd_split) - 1:
                gtid = int(cmd_split[thread_index + 1])
                _, tid = self.router.state_mgr.get_sidtid_by_gtid(gtid)
                cmd_instance.thread_id = gtid
                cmd_split[thread_index + 1] = str(tid)
                cmd_instance.command_no_token = " ".join(cmd_split)
        elif curr_thread := self.router.state_mgr.get_current_gthread():
            cmd_instance.thread_id=curr_thread
            
        # Command handling
        handler = self.command_handlers.get(prefix)
        if not handler:
            asyncio.create_task(self.base_handler.process_command(cmd_instance))
            return
        asyncio.create_task(handler.process_command(cmd_instance))
