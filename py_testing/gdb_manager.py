from typing import List, Optional
from pygdbmi.gdbcontroller import GdbController
from pprint import pprint
from queue import Queue
from threading import Thread
from time import sleep
from utils import *

# BIN_PATH = "../bin/hello_world"

class GdbSession:
    def __init__(self, config: dict) -> None:
        # Ignore addr for now, as we only proceed with localhost
        self.tag = config["tag"]
        self.bin = config["bin"]
        self.args = config["args"]

        if "run_delay" in config.keys():
            self.run_delay = int(config["run_delay"])
        else:
            self.run_delay = None

        self.session_ctrl: Optional[GdbController] = None
        self.mi_output_q: Queue = Queue(maxsize=0)
        self.mi_output_t_handle = None
    
    def start(self):
        full_args = [ "gdb", "--interpreter=mi" , "--args" ]
        full_args.append(self.bin)
        full_args.extend(self.args)
        # full_args.append("--interpreter=mi")
        self.session_ctrl = GdbController(full_args)
        print(f"Started debugging process - \n\ttag: {self.tag}, \n\tbin: {self.bin}, \n\tstartup command: {self.session_ctrl.command}") 

        self.mi_output_t_handle = Thread(
            target=self.fetch_mi_output, args=()
        )
        self.mi_output_t_handle.start()

    def fetch_mi_output(self):
        while True:
            responses = self.session_ctrl.get_gdb_response(timeout_sec=0.5, raise_error_on_timeout=False)
            if responses:
                for r in responses:
                    self.mi_output_q.put_nowait(r)
            sleep(0.1)

    def write(self, cmd: str):
        if (cmd.strip() in [ "run", "r" ]) and self.run_delay:
            sleep(self.run_delay)
        self.session_ctrl.write(cmd, read_response=False)
    
    def deque_mi_output(self) -> dict:
        result = None
        try:
            result = self.mi_output_q.get_nowait()
        except Exception as e:
            pass
        return result

    def get_meta_str(self) -> str:
        return f"[ {self.tag}, {self.bin} ]"

    def cleanup(self):
        print(f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}")
        # self.mi_output_t_handle
        self.session_ctrl.exit()
        
    def __del__(self):
        self.cleanup()
    
class GdbManager:
    def __init__(self, components: List[dict]) -> None:
        self.sessions: List[GdbSession] = []

        for config in components:
            self.sessions.append(GdbSession(config))
        [ s.start() for s in self.sessions ]

        self.output_handle = Thread(target=self.handle_output, args=())
        self.output_handle.start()

    def write(self, cmd: str):
        # pass
        for s in self.sessions:
            s.write(cmd)
        # responses = []
        # for session in self.sessions:
        #     resp = session.write(cmd)
        #     responses.append(resp)

    def handle_output(self):
        while True:
            for s in self.sessions:
                output = s.deque_mi_output()
                if output:
                    meta = s.get_meta_str()
                    # print(f"{meta} {output}")
                    mi_print(output, meta)
            sleep(0.1)

    def cleanup(self):
        print("Cleaning up GdbManager resource")
        for s in self.sessions:
            s.cleanup()

    def __del__(self):
        self.cleanup()
