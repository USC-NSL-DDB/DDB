from enum import Enum
from uuid import uuid4
from typing import List, Optional
from threading import Thread, Lock
from time import sleep
from counter import TSCounter
from response_processor import ResponseProcessor, SessionResponse
from pygdbmi.gdbcontroller import GdbController

from state_manager import StateManager
from gdbserver_starter import RemoteGdbServer, RemoteServerCred
from utils import eprint

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

class AttachMode(Enum):
    LOCAL = 1
    REMOTE = 2

class GdbSession:
    def __init__(self, config: dict, mi_version: str = None) -> None:
        ## Basic information
        self.mi_version: str = mi_version if mi_version else "mi"
        self.tag: str = config["tag"]
        self.args: List[str] = config["args"]

        ## bin will be the binary path combined with cwd
        self.cwd: str = "." if "cwd" not in config.keys() else config["cwd"]
        self.bin: str = os.path.join(self.cwd, config["bin"])

        ## Prepare for the remote mode
        self.remote_cred: RemoteServerCred = None
        self.remote_port: str = None
        self.remote_gdbserver: RemoteGdbServer = None
        self.mode: AttachMode = AttachMode.REMOTE if "mode" in config.keys() and config["mode"] == "remote" else AttachMode.LOCAL
        if self.mode == AttachMode.REMOTE:
            # self.remote_cred = config["cred"]
            self.remote_port = str(config["remote_port"])
            self.remote_cred = RemoteServerCred(
                self.remote_port,
                config["cred"]["hostname"],
                config["cred"]["user"],
                self.bin
            )
            self.remote_gdbserver = RemoteGdbServer(self.remote_cred)

        ## Session metadata
        self.suid = uuid4()
        self.sid = SessionCounter.get()
        self.state_mgr = StateManager.inst()

        if "run_delay" in config.keys():
            self.run_delay: Optional[int] = int(config["run_delay"])
        else:
            self.run_delay = None

        self.session_ctrl: Optional[GdbController] = None
        self.processor = ResponseProcessor.inst()
        self.mi_output_t_handle = None

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
        full_args.extend([ "-ex", f"target remote :{self.remote_port}" ])
        self.session_ctrl = GdbController(full_args)
        # self.session_ctrl.write(f"target remote :{self.remote_port}", read_response=False)
        # print(response)

    def start(self, prerun_cmds: Optional[List[dict]] = None) -> None:
        if self.mode == AttachMode.LOCAL:
            self.local_start(prerun_cmds)
        elif self.mode == AttachMode.REMOTE:
            self.remote_start(prerun_cmds)
        else:
            eprint("Invalid mode")
            return

        print(f"Started debugging process - \n\ttag: {self.tag}, \n\tbin: {self.bin}, \n\tstartup command: {self.session_ctrl.command}") 

        self.state_mgr.register_session(self.sid, self.tag)

        self.mi_output_t_handle = Thread(
            target=self.fetch_mi_output, args=()
        )
        self.mi_output_t_handle.start()

    def fetch_mi_output(self):
        while True:
            responses = self.session_ctrl.get_gdb_response(timeout_sec=0.5, raise_error_on_timeout=False)
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
                        SessionResponse(self.sid, self.get_meta_str(), console_out)
                    )
            # sleep(0.1)

    def write(self, cmd: str):
        token = None
        prefix = None
        cmd_no_token = None
        cmd = cmd.strip()
        for idx, cmd_char in enumerate(cmd):
            if (not cmd_char.isdigit()) and (idx == 0):
                prefix = cmd.split()[0]
                cmd_no_token = cmd
                break
            
            if not cmd_char.isdigit():
                token = cmd[:idx].strip()
                cmd_no_token = cmd[idx:].strip()
                if len(cmd_no_token) == 0:
                    # no meaningful input
                    return
                prefix = cmd_no_token.split()[0]
                break
        cmd = f"{cmd}\n"

        if isinstance(cmd, list):
            self.session_ctrl.write(cmd, read_response=False)
            return

        if (cmd_no_token.strip() in [ "run", "r", "-exec-run" ]) and self.run_delay:
            print("Starts delay")
            sleep(self.run_delay)
            print("Ends delay")
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
        print(f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}")
        # self.mi_output_t_handle
        self.session_ctrl.exit()
        if self.remote_gdbserver:
            self.remote_gdbserver.disconnect()
        
    def __del__(self):
        self.cleanup()
