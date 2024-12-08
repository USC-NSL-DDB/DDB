import asyncio
from threading import Lock, Thread
from typing import List
from time import sleep
from iddb.cmd_processor import CommandProcessor
from iddb.gdbserver_starter import SSHRemoteServerCred
from iddb.mi_formatter import MIFormatter
from iddb.state_manager import StateManager
from iddb.status_server import FlaskApp
from iddb.utils import *
from iddb.cmd_router import CmdRouter
from iddb.service_mgr import ServiceManager
from iddb.gdb_session import GdbMode, GdbSession, GdbSessionConfig, StartMode
from iddb.logging import logger
from iddb.data_struct import GdbCommand, ServiceInfo
from iddb.config import GlobalConfig
from iddb.event_loop import GlobalRunningLoop
from iddb.port_mgr import PortManager
from iddb.gdb_controller import SSHAttachController
from iddb.global_handler import GlobalHandler


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

        GlobalHandler.GDB_SESSION_CLEAN_HANDLE = lambda x: self.remove_session(x)

        self.router = CmdRouter(self.sessions)
        ddbapiserver=FlaskApp(router=self.router)
        Thread(target=ddbapiserver.app.run).start()
        self.processor=CommandProcessor(self.router, GlobalConfig.get().adapter)

        self.state_mgr = StateManager.inst()
        for s in self.sessions:
            s.start()
        ddbapiserver.DDB_up_and_running=True

    def write(self, cmd: str):
        lp=GlobalRunningLoop().get_loop()
        asyncio.run_coroutine_threadsafe(self.processor.send_command(cmd), GlobalRunningLoop().get_loop())

    def __discover_new_session(self, session_info: ServiceInfo):
        # port = PortManager.reserve_port(session_info.ip)
        hostname = session_info.ip
        pid = session_info.pid
        tag = f"{hostname}:-{pid}"
        ddb_conf = GlobalConfig.get()
        prerun_cmds = [
            GdbCommand("async mode", "set mi-async on")
        ]
        prerun_cmds.extend(ddb_conf.prerun_cmds)
        logger.debug(f"New session discovered: hostname={hostname}, pid={pid}, tag={tag}")
        config = GdbSessionConfig(
            # remote_port=port,
            remote_host=hostname,
            gdb_controller=SSHAttachController(
                pid=pid,
                cred=SSHRemoteServerCred(
                    port=ddb_conf.ssh.port,
                    hostname=hostname,
                    username=ddb_conf.ssh.user
                ),
                verbose=True
            ),
            attach_pid=pid,
            tag=tag,
            gdb_mode=GdbMode.REMOTE,
            start_mode=StartMode.ATTACH,
            sudo=ddb_conf.conf.sudo,
            prerun_cmds=prerun_cmds,
            postrun_cmds=ddb_conf.postrun_cmds
        )
        gdb_session = GdbSession(config)

        # 1. add the new session to the session list
        # 2. register router with the new session
        with self.lock:
            self.sessions.append(gdb_session)
        self.router.add_session(gdb_session)

        # start the session: 
        # 1. start ssh to the remote 
        # 2. start a gdb process on the remote and attach to the pid
        try:
            gdb_session.start()
        except Exception as e:
            logger.error(f"Failed to start gdb session: {e}")
            return

    def remove_session(self, sid: int):
        with self.lock:
            for s in self.sessions:
                if s.sid == sid:
                    s.cleanup()
                    self.sessions.remove(s)
                    del self.router.sessions[s.sid]
                    StateManager.inst().remove_session(sid)
                    break
            if len(self.sessions) == 0:
                logger.info("No more sessions. Cleaning up.")
                GlobalHandler.exit_ddb()

    def cleanup(self):
        print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
