import threading
import os
from uuid import uuid4
from typing import List, Optional
from threading import Thread, Lock
from time import sleep
from ddb.counter import TSCounter
from ddb.data_struct import GdbMode, GdbSessionConfig, StartMode
from ddb.response_processor import ResponseProcessor, SessionResponse
from pygdbmi.gdbcontroller import GdbController

from ddb.state_manager import StateManager
from ddb.gdbserver_starter import RemoteServerConnection, SSHRemoteServerClient
from ddb.utils import parse_cmd
from ddb.logging import logger

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
        self.mode: GdbMode = config.gdb_mode
        self.startMode: StartMode = config.start_mode
        self.attach_pid = config.attach_pid
        self.prerun_cmds=config.prerun_cmds
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
        full_args = [ "gdb", self.get_mi_version_arg() ]
        for cmd in self.prerun_cmds:
            full_args.append("-ex")
            full_args.append(cmd["command"])
        full_args.append("--args")
        full_args.append(self.bin)
        full_args.extend(self.args)
        self.session_ctrl = GdbController(full_args)

    def remote_attach(self):
        logger.debug("start remote attach")
        if not self.remote_gdbserver:
            logger.warn("Remote gdbserver not initialized")
            return

        if isinstance(self.remote_gdbserver, SSHRemoteServerClient):
            self.remote_gdbserver.start(attach_pid=self.attach_pid, sudo=self.sudo)
        else:
            self.remote_gdbserver.connect()
            command = ["gdbserver", f":{self.remote_port}", "--attach", f"{str(self.attach_pid)}"]
            logger.debug(f"gdbserver command: {command}")
            output = self.remote_gdbserver.execute_command_async(command)
            logger.debug(output)
            logger.debug("finish attach")

        gdb_cmd = ["gdb", self.get_mi_version_arg(), "-q"]
        self.session_ctrl = GdbController(gdb_cmd)

        self.write("-gdb-set mi-async on")
        # https://github.com/USC-NSL/distributed-debugger/issues/62
        # Workaround for async+all-stop mode for gdbserver
        self.write("maint set target-non-stop on")
        self.write("-gdb-set non-stop off")

        for prerun_cmd in self.prerun_cmds:
            self.write(f'-interpreter-exec console "{prerun_cmd["command"]}"')
        self.write(f"-target-select remote {self.remote_host}:{self.remote_port}")

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
        self.write(f"-target-select remote {self.remote_host}:{self.remote_port}")

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

        logger.debug(
            f"Started debugging process - \n\ttag: {self.tag}, \n\tbin: {self.bin}, \n\tstartup command: {self.session_ctrl.command}"
        )

        self.state_mgr.register_session(self.sid, self.tag)

        self.mi_output_t_handle = Thread(
            target=self.fetch_mi_output, args=()
        )
        self.mi_output_t_handle.start()

    def fetch_mi_output(self):
        while not self._stop_event.is_set():
            responses = self.session_ctrl.get_gdb_response(
                timeout_sec=0.5, raise_error_on_timeout=False)
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
        if ("-exec-interrupt" == cmd_no_token.strip()) and self.StartMode == StartMode.ATTACH:
            logger.debug(f"{self.sid} sending kill to",self.attach_pid)
            self.remote_gdbserver.execute_command(["kill", "-5", str(self.attach_pid)])
            return

        self.session_ctrl.write(cmd, read_response=False)

    def get_meta_str(self) -> str:
        return f"[ {self.tag}, {self.bin}, {self.sid} ]"

    def cleanup(self):
        self._stop_event.set()
        sleep(1) # wait to let fetch thread to stop
        logger.debug(
            f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}"
        )
        try:
            # try-except in case the gdb is already killed or exited.
            response = self.session_ctrl.write("kill", read_response=True)
            logger.debug(f"kill response: {response}")
            self.session_ctrl.exit()
        except Exception as e:
            logger.debug(f"Failed to clean up gdb: {e}")
        if self.remote_gdbserver:
            self.remote_gdbserver.close()

    def __del__(self):
        self.cleanup()
