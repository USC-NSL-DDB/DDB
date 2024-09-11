import threading
import os
import time
from uuid import uuid4
from typing import List, Optional
from threading import Thread, Lock
from time import sleep
from ddb.counter import TSCounter
from ddb.data_struct import GdbMode, GdbSessionConfig, StartMode
from ddb.gdb_controller import RemoteGdbController
from ddb.gdbparser import GdbParser
from ddb.response_processor import ResponseProcessor, SessionResponse
from pygdbmi.gdbcontroller import GdbController

from ddb.state_manager import StateManager
from ddb.gdbserver_starter import RemoteServerConnection, SSHRemoteServerClient
from ddb.utils import parse_cmd
from ddb.logging import logger
from ddb.config import DevFlags

class SessionCounter:
    _sc: "SessionCounter" = None
    _lock = Lock()

    def __init__(self) -> None:
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "SessionCounter":
        with SessionCounter._lock:
            if SessionCounter._sc:
                return SessionCounter._sc
            SessionCounter._sc = SessionCounter()
            return SessionCounter._sc

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return SessionCounter.inst().inc()

class GdbSession:
    def __init__(self, config: GdbSessionConfig, mi_version: str = None) -> None:
        # Basic information
        self.mi_version: str = mi_version if mi_version else "mi3"
        self.tag: str = config.tag
        self.args: List[str] = config.args

        # bin will be the binary path combined with cwd
        self.cwd: str = config.cwd
        self.bin: str = os.path.join(self.cwd, config.binary)

        # Prepare for the remote mode
        self.remote_host: str = config.remote_host
        self.remote_port: str = str(config.remote_port)
        self.remote_gdbserver: RemoteServerConnection = config.remote_gdbserver
        self.gdb_controller:RemoteGdbController=config.gdb_controller
        self.gdb_response_parser=GdbParser()
        self.mode: GdbMode = config.gdb_mode
        self.startMode: StartMode = config.start_mode
        self.attach_pid = config.attach_pid
        self.prerun_cmds=config.prerun_cmds
        self.initialize_commands = config.initialize_commands
        self.sudo = config.sudo

        # Session metadata
        self.suid = uuid4()
        self.sid = SessionCounter.get()
        self.state_mgr = StateManager.inst()

        self.run_delay = config.run_delay

        self.session_ctrl: Optional[GdbController] = None
        self.processor = ResponseProcessor.inst()
        self.mi_output_t_handle = None
        self._stop_event = threading.Event()

    def get_mi_version_arg(self) -> str:
        return f"--interpreter={self.mi_version}"

    def local_start(self):
        full_args = [ "gdb", self.get_mi_version_arg(), "-q" ]
        full_args.append("--args")
        full_args.append(self.bin)
        full_args.extend(self.args)
        self.session_ctrl = GdbController(full_args)

        for prerun_cmd in self.prerun_cmds:
            self.write(f'-interpreter-exec console "{prerun_cmd["command"]}"')

        self.write("-gdb-set mi-async on")
        self.write("-gdb-set non-stop off")

    def remote_attach(self):
        logger.debug("start remote attach")
        if not self.gdb_controller:
            logger.warn("Remote gdbcontroller not initialized")
            return
        gdb_cmd = " ".join(["gdb", self.get_mi_version_arg(), "-q"])
        self.gdb_controller.start(gdb_cmd)

        self.gdb_controller.write_input("-gdb-set mi-async on")

        for prerun_cmd in self.prerun_cmds:
            self.gdb_controller.write_input(f'-interpreter-exec console "{prerun_cmd["command"]}"')
        for init_cmd in self.initialize_commands:
            self.write(init_cmd)
        self.write(f"-target-attach {self.attach_pid}")
        # self.write(f"-file-exec-and-symbols /proc/{self.attach_pid}/root{self.bin}")
        self.write(f"-gdb-set logging enabled on")
            
    def remote_start(self):
        if not self.remote_gdbserver:
            logger.warn("Remote gdbserver not initialized")
            return
        
        self.remote_gdbserver.start(self.args, sudo=self.sudo)
        gdb_cmd = [ "gdb", self.get_mi_version_arg(), "-q" ]
        self.session_ctrl = GdbController(gdb_cmd)

        self.write("-gdb-set mi-async on")
        # https://github.com/USC-NSL/distributed-debugger/issues/62
        # Workaround for async+all-stop mode for gdbserver
        self.write("maint set target-non-stop on")
        self.write("-gdb-set non-stop off")
        
        for prerun_cmd in self.prerun_cmds:
            self.write(prerun_cmd["command"])

        if DevFlags.USE_EXTENDED_REMOTE:
            self.write(f"-target-select extended-remote {self.remote_host}:{self.remote_port}")
        else:
            self.write(f"-target-select remote {self.remote_host}:{self.remote_port}")

        if DevFlags.USE_EXTENDED_REMOTE:
            self.write(f"set remote exec-file {self.bin}")
            self.write(f"set args {' '.join(self.args)}")


    def start(self) -> None:
        if self.mode == GdbMode.LOCAL:
            self.local_start()
        elif self.mode == GdbMode.REMOTE:
            if self.startMode == StartMode.ATTACH:
                self.remote_attach()
            elif self.startMode == StartMode.BINARY:
                self.remote_start()
        else:
            logger.error("Invalid mode")
            return

        # logger.debug(
        #     f"Started debugging process - \n\ttag: {self.tag}, \n\tbin: {self.bin}, \n\tstartup command: {self.session_ctrl.command}"
        # )

        self.state_mgr.register_session(self.sid, self.tag,self)

        self.mi_output_t_handle = Thread(
            target=self.fetch_mi_output, args=()
        )
        self.mi_output_t_handle.start()

    def fetch_mi_output(self):
        while not self._stop_event.is_set() and self.gdb_controller.is_open():
            response = self.gdb_controller.fetch_output(
                timeout=1)
            # logger.debug(f"raw response from session [{self.sid}] ,{response}")
            responses=self.gdb_response_parser.get_responses_list(response,"stdout")
            # logger.debug(f"parsed response from gdb,${responses}")
            if responses:
                payload = ""
                for r in responses:
                    if r["type"] == "console":
                        payload += r["payload"]
                    else:
                        self.processor.put(
                            SessionResponse(self.sid, self.get_meta_str(), r)
                        )

                console_out = {
                    "type": "console",
                    "message": None,
                    "stream": "stdout",
                    "payload": None
                }
                payload = payload.strip()
                if payload:
                    console_out["payload"] = payload
                    self.processor.put(
                        SessionResponse(
                            self.sid, self.get_meta_str(), console_out)
                    )
            # logger.debug(f"raw response from session [{self.sid}] ,{response}")

    def write(self, cmd: str):
        _, cmd_no_token, _, cmd = parse_cmd(cmd)

        # TODO: check if removing support of a list of commands is okay?
        # if isinstance(cmd, list):
            # cmd=" ".join(cmd)
        logger.debug(f"send command to session {self.sid}:\n{cmd}")
        if (cmd_no_token.strip() in [ "run", "r", "-exec-run" ]) and self.run_delay:
            sleep(self.run_delay)
        
        # Special case for handling interruption when child process is spawned.
        # `exec-interrupt` won't work in this case. Need manually send kill signal.
        # TODO: how to handle this elegantly?
        if ("-exec-interrupt" == cmd_no_token.strip()) and self.startMode == StartMode.ATTACH:
            logger.debug(f"session {self.sid} sending kill to {self.attach_pid}")
            self.remote_gdbserver.execute_command(["kill", "-5", str(self.attach_pid)])
            return

        self.gdb_controller.write_input(cmd)

    def get_meta_str(self) -> str:
        return f"[ {self.tag}, {self.bin}, {self.sid} ]"

    def cleanup(self):
        self._stop_event.set()
        sleep(1) # wait to let fetch thread to stop should be larger than timeout
        logger.debug(
            f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}"
        )
        if self.gdb_controller.is_open():
            if self.startMode==StartMode.BINARY:
            # try-except in case the gdb is already killed or exited.
                self.gdb_controller.write_input("kill")
            else:
                self.gdb_controller.write_input("detach")
            self.gdb_controller.write_input("exit")
            self.gdb_controller.close()
    def __del__(self):
        self.cleanup()
