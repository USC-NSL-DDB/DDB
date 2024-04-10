from enum import Enum
from uuid import uuid4
from typing import List, Optional
from threading import Thread, Lock
from time import sleep
from counter import TSCounter
from response_processor import ResponseProcessor, SessionResponse
from pygdbmi.gdbcontroller import GdbController

from state_manager import StateManager
from gdbserver_starter import RemoteServerConnection
from utils import eprint, parse_cmd
from dataclasses import dataclass, field
import os

# A simple wrapper around counter in case any customization later


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


class GdbMode(Enum):
    LOCAL = 1
    REMOTE = 2


class StartMode(Enum):
    BINARY = 1
    ATTACH = 2


@dataclass
class GdbSessionConfig:
    remote_port: int = -1
    remote_host: str = ""
    remote_gdbserver: RemoteServerConnection = None
    attach_pid: int = -1
    binary: str = ""
    tag: Optional[str] = None
    gdb_mode: GdbMode = 1
    start_mode: StartMode = 1
    mi_version: str = "mi"
    cwd: str = "."
    # Using default_factory for mutable default
    args: List[str] = field(default_factory=list)
    run_delay: int = 0
    gdb_config_cmds:List[str] = field(default_factory=list)

class GdbSession:
    def __init__(self, config: GdbSessionConfig, mi_version: str = None) -> None:
        # Basic information
        self.mi_version: str = mi_version if mi_version else "mi"
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

        # Session metadata
        self.suid = uuid4()
        self.sid = SessionCounter.get()
        self.state_mgr = StateManager.inst()

        self.run_delay = config.run_delay

        self.session_ctrl: Optional[GdbController] = None
        self.processor = ResponseProcessor.inst()
        self.mi_output_t_handle = None
        self.gdb_config_cmds = config.gdb_config_cmds

    def get_mi_version_arg(self) -> str:
        return f"--interpreter={self.mi_version}"

    def local_start(self, prerun_cmds: Optional[List[dict]] = None):
        full_args = [ "gdb", self.get_mi_version_arg() ]
        if prerun_cmds:
            for cmd in prerun_cmds:
                full_args.append("-ex")
                full_args.append(cmd["command"])
        full_args.append("--args")
        full_args.append(self.bin)
        full_args.extend(self.args)
        self.session_ctrl = GdbController(full_args)

    def remote_attach(self, prerun_cmds: Optional[List[dict]] = None):
        print("start remote attach")
        if not self.remote_gdbserver:
            eprint("Remote gdbserver not initialized")
            return

        # self.remote_gdbserver.connect()
        # command = ["gdbserver", f":{self.remote_port}", "--attach", f"{str(self.attach_pid)}"]
        # print("gdbserver command", command)
        # output = self.remote_gdbserver.execute_command_async(command)
        # print(output)
        # print("finish attach")
        # gdb_cmd = ["gdb", self.get_mi_version_arg(),"-q"]
        # self.session_ctrl = GdbController(
        #     gdb_cmd
        # )
        # self.write("-gdb-set mi-async on")
        # for gdb_condig_cmd in self.gdb_config_cmds:
        #     self.write(f'-interpreter-exec console "{gdb_condig_cmd}"')
        # self.write(f"-target-select remote {self.remote_host}:{self.remote_port}")

        self.remote_gdbserver.start(self.args, attach_pid=self.attach_pid)
        full_args = [ "gdb", self.get_mi_version_arg() ]
        if prerun_cmds:
            for cmd in prerun_cmds:
                full_args.append("-ex")
                full_args.append(cmd["command"])
        full_args.extend([ "-ex", f"-target-select remote {self.remote_host}:{self.remote_port}" ])
        self.session_ctrl = GdbController(full_args)

    def remote_start(self, prerun_cmds: Optional[List[dict]] = None):
        if not self.remote_gdbserver:
            eprint("Remote gdbserver not initialized")
            return
        
        self.remote_gdbserver.start(self.args)
        full_args = [ "gdb", self.get_mi_version_arg() ]
        if prerun_cmds:
            for cmd in prerun_cmds:
                full_args.append("-ex")
                full_args.append(cmd["command"])
        full_args.extend([ "-ex", f"target remote {self.remote_host}:{self.remote_port}" ])
        self.session_ctrl = GdbController(full_args)
        # self.session_ctrl.write(f"target remote :{self.remote_port}", read_response=False)
        # print(response)

    def start(self, prerun_cmds: Optional[List[dict]] = None) -> None:
        if self.mode == GdbMode.LOCAL:
            self.local_start(prerun_cmds)
        elif self.mode == GdbMode.REMOTE:
            if self.startMode == StartMode.ATTACH:
                self.remote_attach(prerun_cmds)
            elif self.startMode == StartMode.BINARY:
                self.remote_start(prerun_cmds)
        else:
            eprint("Invalid mode")
            return

        print(
            f"Started debugging process - \n\ttag: {self.tag}, \n\tbin: {self.bin}, \n\tstartup command: {self.session_ctrl.command}")

        self.state_mgr.register_session(self.sid, self.tag)

        self.mi_output_t_handle = Thread(
            target=self.fetch_mi_output, args=()
        )
        self.mi_output_t_handle.start()

    def fetch_mi_output(self):
        while True:
            responses = self.session_ctrl.get_gdb_response(
                timeout_sec=0.5, raise_error_on_timeout=False)
            if responses:
                print(f"raw response from{self.sid}",responses)
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
            # sleep(0.1)

    def write(self, cmd: str):
        _, cmd_no_token, _, cmd = parse_cmd(cmd)

        # TODO: check if removing support of a list of commands is okay?
        # if isinstance(cmd, list):
            # cmd=" ".join(cmd)
        print(f"send command {cmd} to session {self.sid}")
        if (cmd_no_token.strip() in [ "run", "r", "-exec-run" ]) and self.run_delay:
            sleep(self.run_delay)
        if ("-exec-interrupt" in cmd_no_token.strip() ) and self.startMode==StartMode.ATTACH:
            print(f"{self.sid} sending kill to",self.attach_pid)
            self.remote_gdbserver.execute_command(["kill", "-5", str(self.attach_pid)])
            return
        self.session_ctrl.write(cmd, read_response=False)

    # def deque_mi_output(self) -> dict:
    #     result = None
    #     try:
    #         result = self.mi_output_q.get_nowait()
    #     except Exception as e:
    #         pass
    #     return result

    def get_meta_str(self) -> str:
        return f"[ {self.tag}, {self.bin}, {self.sid} ]"

    def cleanup(self):
        print(
            f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}")
        # self.mi_output_t_handle
        self.session_ctrl.exit()
        if self.remote_gdbserver:
            self.remote_gdbserver.close()

    def __del__(self):
        self.cleanup()
