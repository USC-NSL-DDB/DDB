from threading import Lock, Thread
from queue import Queue
from utils import mi_print
from state_manager import StateManager, ThreadStatus

class SessionResponse:
    def __init__(self, sid: int, meta: str, response: dict) -> None:
        self.sid = sid
        self.meta = meta
        self.response = response

    def __str__(self) -> str:
        return f"Response - sid: {self.sid}, output:\n\t{self.output}"

class ResponseProcessor:
    _instance: "ResponseProcessor" = None 
    _lock = Lock()

    def __init__(self) -> None:
        self.queue: Queue[SessionResponse] = Queue(maxsize=0)
        self.state_manager = StateManager.inst()
        self.process_handle = Thread(
            target=self.process, args=()
        )
        self.process_handle.start()

    @staticmethod
    def inst() -> "ResponseProcessor":
        with ResponseProcessor._lock:
            if ResponseProcessor._instance:
                return ResponseProcessor._instance
            ResponseProcessor._instance = ResponseProcessor()
            return ResponseProcessor._instance

    def put(self, response: SessionResponse):
        self.queue.put_nowait(response)

    def process(self):
        while True:
            resp = self.queue.get()
            resp_type = resp.response["type"]

            # Special handling for different types of response
            mi_print(resp.response, resp.meta) 

            if resp_type == "notify":
                self.handle_notify(resp)
                print(str(self.state_manager))

    def handle_notify(self, response: SessionResponse):
        sid = response.sid
        resp_msg = response.response["message"]
        resp_payload = response.response["payload"]

        if resp_msg == "thread-created":
            tgid = str(resp_payload["group-id"])
            self.state_manager.create_thread(sid, int(resp_payload["id"]), tgid)
        elif resp_msg == "running":
            thread_id = resp_payload["thread-id"]
            if thread_id == "all":
                self.state_manager.update_all_thread_status(sid, ThreadStatus.RUNNING)
            else:
                thread_id = int(thread_id)
                self.state_manager.update_thread_status(sid, thread_id, ThreadStatus.RUNNING)
        elif resp_msg == "stopped":
            thread_id = resp_payload["thread-id"]
            if thread_id == "all":
                self.state_manager.update_all_thread_status(sid, ThreadStatus.STOPPED)
            else:
                thread_id = int(thread_id)
                self.state_manager.update_thread_status(sid, thread_id, ThreadStatus.STOPPED)
                # Here, we assume it runs in all-stop mode. 
                # Therefore, when a thread hits a breakpoint, 
                # all threads stops and the currently stopped thread 
                # as the current selected thread automatically.
                self.state_manager.set_current_tid(sid, thread_id)

            stopped_threads = resp_payload["stopped-threads"]
            if stopped_threads == "all":
                self.state_manager.update_all_thread_status(sid, ThreadStatus.STOPPED)
            else:
                # In non-stop modes, we need to handle a list of threads as they may stop at different times.
                for t in stopped_threads:
                    tid = int(t)
                    self.state_manager.update_thread_status(sid, tid, ThreadStatus.STOPPED)
        elif resp_msg == "thread-group-started":
            tgid = str(resp_payload['id'])
            pid = int(resp_payload["pid"])
            self.state_manager.start_thread_group(sid, tgid, pid)
        elif resp_msg == "thread-group-exited":
            tgid = str(resp_payload['id'])
            self.state_manager.exit_thread_group(sid, tgid)
        else:
            print("Ignoring this notify record for now.")

# Eager instantiation
_ = ResponseProcessor.inst()
