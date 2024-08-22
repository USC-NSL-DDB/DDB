import asyncio
from threading import Lock, Thread
from typing import List, Optional
from time import sleep
from ddb.cmd_processor import CommandProcessor
from ddb.gdbserver_starter import SSHRemoteServerCred, SSHRemoteServerClient
from ddb.state_manager import StateManager
from ddb.status_server import FlaskApp
from ddb.utils import *
from ddb.cmd_router import CmdRouter
from ddb.service_mgr import ServiceManager
from ddb.gdb_session import GdbMode, GdbSession, GdbSessionConfig, StartMode
from ddb.logging import logger
from ddb.data_struct import ServiceInfo
from ddb.config import GlobalConfig
from ddb.event_loop import GlobalRunningLoop


class GdbManager:
    def __init__(self) -> None:
        self.lock = Lock()
        self.sessions: List[GdbSession] = []

    def start(self)->None:
        # start a global running loop for asyncio context
        _ = GlobalRunningLoop()
        global_config = GlobalConfig.get()
        if global_config.broker:
            logger.debug("Broker is enabled. Starting ServiceManager.")
            self.service_mgr: ServiceManager = ServiceManager()
            self.service_mgr.set_callback_on_new_service(self.__discover_new_session)

        for config in global_config.gdb_sessions_configs:
            self.sessions.append(GdbSession(config))

        self.router = CmdRouter(self.sessions)
        ddbapiserver=FlaskApp(router=self.router)
        Thread(target=ddbapiserver.app.run).start()
        self.processor=CommandProcessor(self.router)
        self.state_mgr = StateManager.inst()
        for s in self.sessions:
            s.start()
        ddbapiserver.DDB_up_and_running=True
    def write(self, cmd: str):
        # asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), GlobalRunningLoop().get_loop())
        lp=GlobalRunningLoop().get_loop()
        # logger.debug(f"Sending command: {cmd} {len(asyncio.all_tasks(lp))} {lp} {lp.is_running()}")
        asyncio.run_coroutine_threadsafe(self.processor.send_command(cmd), GlobalRunningLoop().get_loop())

    def __discover_new_session(self, session_info: ServiceInfo):
        logger.debug(f"In GdbManager. New session discovered: {session_info}")
        port = 8989
        hostname = session_info.ip
        username = "ybyan"
        pid = session_info.pid
        tag = session_info.tag
        config = GdbSessionConfig(
            remote_port=port,
            remote_host=hostname,
            username=username,
            remote_gdbserver=SSHRemoteServerClient(
                cred=SSHRemoteServerCred(
                    port=port,
                    bin="",
                    hostname=hostname,
                    username=username
                )
            ),
            attach_pid=pid,
            tag=tag,
            gdb_mode=GdbMode.REMOTE,
            start_mode=StartMode.ATTACH,
            sudo=True,
            prerun_cmds=[
                {
                    "name": "async mode",
                    "command": "set mi-async on"
                }
            ]
        )
        gdb_session = GdbSession(config)

        # 1. add the new session to the session list
        # 2. register router with the new session
        with self.lock:
            self.sessions.append(gdb_session)
        self.router.add_session(gdb_session)

        # start the session: 
        # 1. start gdbserver on the remote 
        # 2. start local gdb process and attach
        gdb_session.start()

    def cleanup(self):
        print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
