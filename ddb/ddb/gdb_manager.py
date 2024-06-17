import asyncio
from threading import Lock
from typing import List, Optional
from time import sleep
from ddb.gdbserver_starter import SSHRemoteServerCred, SSHRemoteServerClient
from ddb.state_manager import StateManager
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
        # _ = GlobalRunningLoop()
        global_config = GlobalConfig.get()
        if global_config.broker:
            logger.debug("Broker is enabled. Starting ServiceManager.")
            self.service_mgr: ServiceManager = ServiceManager()
            self.service_mgr.set_callback_on_new_service(self.__discover_new_session)

        for config in global_config.gdb_sessions_configs:
            self.sessions.append(GdbSession(config))

        self.router = CmdRouter(self.sessions)
        self.state_mgr = StateManager.inst()

        [ s.start() for s in self.sessions ]

    def write(self, cmd: str):
        # if cmd.strip() and cmd.split()[0] == "session":
        #     selection = int(cmd.split()[1])
        #     self.state_mgr.set_current_session(selection)
        #     dev_print(f"selected session {self.state_mgr.get_current_session()}.")
        # else:
        #asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), self.router.loop).result()

        # asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), self.router.event_loop_thread.loop)
        asyncio.run_coroutine_threadsafe(self.router.send_cmd(cmd), GlobalRunningLoop.inst().get_loop())

        # for s in self.sessions:
        #     s.write(cmd)

        # responses = []
        # for session in self.sessions:
        #     resp = session.write(cmd)
        #     responses.append(resp)

    def __discover_new_session(self, session_info: ServiceInfo):
        logger.debug(f"In GdbManager. New session discovered: {session_info}")
        port = 8989
        hostname = session_info.ip
        username = "ybyan"
        pid = session_info.pid
        tag = f"{hostname}:-{pid}"
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
        dev_print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
