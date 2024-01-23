from uuid import uuid4
from state_manager import StateManager
from typing import List, Optional
from pygdbmi.gdbcontroller import GdbController
from threading import Thread
from time import sleep
from threading import Lock
from counter import TSCounter
from response_processor import ResponseProcessor, SessionResponse

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

class GdbSession:
    def __init__(self, config: dict) -> None:
        # Ignore addr for now, as we only proceed with localhost
        self.tag = config["tag"]
        self.bin = config["bin"]
        self.args = config["args"]
        self.suid = uuid4()
        self.sid = SessionCounter.get()
        self.state_mgr = StateManager.inst()

        if "run_delay" in config.keys():
            self.run_delay = int(config["run_delay"])
        else:
            self.run_delay = None

        self.session_ctrl: Optional[GdbController] = None
        self.processor = ResponseProcessor.inst()
        # self.mi_output_q: Queue = Queue(maxsize=0)
        self.mi_output_t_handle = None
    
    def start(self):
        full_args = [ "gdb", "--interpreter=mi" , "--args" ]
        full_args.append(self.bin)
        full_args.extend(self.args)
        # full_args.append("--interpreter=mi")
        self.session_ctrl = GdbController(full_args)
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
        if (cmd.strip() in [ "run", "r" ]) and self.run_delay:
            sleep(self.run_delay)
        self.session_ctrl.write(cmd, read_response=False)
    
    # def deque_mi_output(self) -> dict:
    #     result = None
    #     try:
    #         result = self.mi_output_q.get_nowait()
    #     except Exception as e:
    #         pass
    #     return result

    def get_meta_str(self) -> str:
        return f"[ {self.tag}, {self.bin} ]"

    def cleanup(self):
        print(f"Exiting gdb/mi controller - \n\ttag: {self.tag}, \n\tbin: {self.bin}")
        # self.mi_output_t_handle
        self.session_ctrl.exit()
        
    def __del__(self):
        self.cleanup()
