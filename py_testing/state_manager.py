from typing import List, Optional
# from gdb_manager import GdbSession
from uuid import uuid4, UUID
from enum import Enum
from threading import Lock, RLock

class ThreadStatus(Enum):
    INIT = 1
    STOPPED = 2
    RUNNING = 3
    TERMINATED = 4

class SessionMeta:
    def __init__(self, sid: int, tag: str) -> None:
        self.tag = tag
        self.sid = sid
        self.current_tid: Optional[int] = None
        self.t_status: dict[int, ThreadStatus] = {}
        
        self.t_to_tg: dict[int, str] = {}
        self.tg_to_t: dict[str, set[int]] = {}

        self.rlock = RLock()

    def create_thread(self, tid: int):
        self.t_status[tid] = ThreadStatus.INIT

    def create_thread_group(self, tgid: str):
        with self.rlock:
            if not (tgid in self.tg_to_t):
                self.tg_to_t[tgid] = {}

    def add_thread_to_group(self, tid: int, tgid: str):
        with self.rlock:
            if not (tgid in self.tg_to_t):
                self.create_thread_group(tgid)
        
            self.tg_to_t[tgid].add(tid)
            self.t_to_tg[tid] = tgid

    # def remove_thread(self, tid: int):
    #     tgid = self.t_to_tg[tid]
    #     self.tg_to_t[tgid].remove(tid)
    #     del self.t_to_tg[tid]

    def update_t_status(self, tid: int, new_status: ThreadStatus):
        with self.rlock:
            self.t_status[tid] = new_status

    def update_all_status(self, new_status: ThreadStatus):
        with self.rlock:
            for t in self.t_status:
                self.t_status[t] = new_status

    def set_current_tid(self, tid: int):
        with self.rlock:
            self.current_tid = tid

    def __str__(self) -> str:
        out = f"[ SessionMeta - sid: {self.sid}, tag: {self.tag} ]\n\t"
        out += f"current thread id: {self.current_tid}\n\t"
        out += "thread status: "
        for ts in self.t_status:
            out += f"({ts}, {self.t_status[ts]}), "
        out += f"\n\tthread to thread group: {self.t_to_tg}"
        out += f"\n\tthread group to thread: {self.tg_to_t}\n"
        return out

class StateManager:
    _store: "StateManager" = None
    _lock = Lock()

    def __init__(self) -> None:
        self.sessions: dict[int, SessionMeta] = {}

    @staticmethod
    def inst() -> "StateManager":
        with StateManager._lock:
            if StateManager._store:
                return StateManager._store 
            StateManager._store = StateManager()
            return StateManager._store

    def register_session(self, sid: int, tag: str):
        self.sessions[sid] = SessionMeta(sid, tag)

    def create_thread(self, sid: int, tid: int):
        self.sessions[sid].create_thread(tid)

    def update_thread_status(self, sid: int, tid: int, status: ThreadStatus):
        self.sessions[sid].update_t_status(tid, status)

    def update_all_thread_status(self, sid: int, status: ThreadStatus):
        self.sessions[sid].update_all_status(status)

    def set_current_tid(self, sid: int, current_tid: int):
        self.sessions[sid].set_current_tid(current_tid)

    def __str__(self) -> str:
        out = "Session States: \n"
        for sid in self.sessions:
            out += f"sid: {sid}, {self.sessions[sid]}"
        return out

# Eager instantiation
_ = StateManager.inst()
