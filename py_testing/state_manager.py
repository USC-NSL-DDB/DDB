from typing import List, Optional, Tuple
# from gdb_manager import GdbSession
from uuid import uuid4, UUID
from enum import Enum
from threading import Lock, RLock

class ThreadStatus(Enum):
    INIT = 1
    STOPPED = 2
    RUNNING = 3
    # TERMINATED = 4

class ThreadGroupStatus(Enum):
    INIT = 1
    STOPPED = 2
    RUNNING = 3
    EXITED = 4

class SessionMeta:
    def __init__(self, sid: int, tag: str) -> None:
        self.tag = tag
        self.sid = sid
        self.current_tid: Optional[int] = None
        self.t_status: dict[int, ThreadStatus] = {}

        # maps session unique tid to per inferior tid
        # for example, if session 1 has:
        # tg1: { 1, 2, 4 }
        # tg2: { 3 } then,
        # self.tid_to_per_inferior_tid = { 1: 1, 2: 1, 3: 2, 4: 1 }
        self.tid_to_per_inferior_tid: dict[int, int] = {}
        
        # maps thread_id (int) to its belonging thread_group_id (str)
        self.t_to_tg: dict[int, str] = {}
        # maps thread_group_id (str) to its owning (list of) thread_id (int)
        self.tg_to_t: dict[str, set[int]] = {}

        # maps thread_group_id (str) to ThreadGroupStatus
        self.tg_status: dict[str, ThreadGroupStatus] = {}
        # maps thread_group_id (str) to pid that thread group represents
        self.tg_to_pid: dict[str, int] = {}

        self.rlock = RLock()

    def create_thread(self, tid: int, tgid: str):
        with self.rlock:
            self.t_status[tid] = ThreadStatus.INIT
            self.t_to_tg[tid] = tgid
            
            num_exist_threads = len(self.tg_to_t[tgid])
            # manage the per-inferior thread id
            self.tid_to_per_inferior_tid[tid] = num_exist_threads + 1
            self.tg_to_t[tgid].add(tid)

    def add_thread_group(self, tgid: str):
        with self.rlock:
            if not (tgid in self.tg_to_t):
                self.tg_to_t[tgid] = set()
            self.tg_status[tgid] = ThreadGroupStatus.INIT

    def start_thread_group(self, tgid: str, pid: int):
        with self.rlock:
            # self.create_thread_group(tgid)
            self.tg_status[tgid] = ThreadGroupStatus.RUNNING
            self.tg_to_pid[tgid] = pid

    def exit_thread_group(self, tgid: str):
        with self.rlock:
            self.tg_status[tgid] = ThreadGroupStatus.EXITED
            # Also clean up all threads belongs to that thread group
            threads = self.tg_to_t[tgid]
            for t in threads:
                del self.t_to_tg[t]
                del self.t_status[t]
            self.tg_to_t[tgid].clear()

    def add_thread_to_group(self, tid: int, tgid: str):
        with self.rlock:
            if not (tgid in self.tg_to_t):
                self.add_thread_group(tgid)
        
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
        out += f"\n\tthread group to thread: {self.tg_to_t}"
        out += f"\n\ttrhead group status: {self.tg_status}"
        out += f"\n\ttid to per thread group tid: {self.tid_to_per_inferior_tid}"
        return out

# A simple wrapper around counter in case any customization later
class GlobalInferiorIdCounter:
    _c: "GlobalInferiorIdCounter" = None
    _lock = Lock()
    
    def __init__(self) -> None:
        from counter import TSCounter
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "GlobalInferiorIdCounter":
        with GlobalInferiorIdCounter._lock:
            if GlobalInferiorIdCounter._c:
                return GlobalInferiorIdCounter._c
            GlobalInferiorIdCounter._c = GlobalInferiorIdCounter()
            return GlobalInferiorIdCounter._c

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return GlobalInferiorIdCounter.inst().inc()

# A simple wrapper around counter in case any customization later
class GlobalThreadIdCounter:
    _c: "GlobalThreadIdCounter" = None
    _lock = Lock()
    
    def __init__(self) -> None:
        from counter import TSCounter
        self.counter = TSCounter()

    @staticmethod
    def inst() -> "GlobalThreadIdCounter":
        with GlobalThreadIdCounter._lock:
            if GlobalThreadIdCounter._c:
                return GlobalThreadIdCounter._c
            GlobalThreadIdCounter._c = GlobalThreadIdCounter()
            return GlobalThreadIdCounter._c

    def inc(self) -> int:
        return self.counter.increment()

    @staticmethod
    def get() -> int:
        return GlobalThreadIdCounter.inst().inc()

class StateManager:
    _store: "StateManager" = None
    _lock = Lock()

    def __init__(self) -> None:
        self.sessions: dict[int, SessionMeta] = {}
        self.current_session = None
        self.selected_gthread = None

        # Maps (session + thread id) to global thread id
        self.sidtid_to_gtid: dict[Tuple[int, int], int] = {}
        # Maps global thread id to (session + thread id)
        self.gtid_to_sidtid: dict[int, Tuple[int, int]] = {}
        
        # Maps (session + thread group id) to global inferior id
        self.sidtgid_to_giid: dict[Tuple[int, str], int] = {}
        # Maps global inferior id to (session + thread group id)
        self.giid_to_sidtgid: dict[int, Tuple[int, str]] = {}

        self.lock = RLock()

    @staticmethod
    def inst() -> "StateManager":
        with StateManager._lock:
            if StateManager._store:
                return StateManager._store 
            StateManager._store = StateManager()
            return StateManager._store

    def register_session(self, sid: int, tag: str):
        self.sessions[sid] = SessionMeta(sid, tag)

    def add_thread_group(self, sid: int, tgid: str):
        with self.lock:
            giid = GlobalInferiorIdCounter.get()
            self.sidtgid_to_giid[(sid, tgid)] = giid
            self.giid_to_sidtgid[giid] = (sid, tgid)

        self.sessions[sid].add_thread_group(tgid)

    def start_thread_group(self, sid: int, tgid: str, pid: int):
        self.sessions[sid].start_thread_group(tgid, pid)
    
    def exit_thread_group(self, sid: int, tgid: str):
        self.sessions[sid].exit_thread_group(tgid)

    def create_thread(self, sid: int, tid: int, tgid: str):
        with self.lock:
            gtid = GlobalThreadIdCounter.get()
            self.sidtid_to_gtid[(sid, tid)] = gtid
            self.gtid_to_sidtid[gtid] = (sid, tid)
        self.sessions[sid].create_thread(tid, tgid)

    def update_thread_status(self, sid: int, tid: int, status: ThreadStatus):
        self.sessions[sid].update_t_status(tid, status)

    def update_all_thread_status(self, sid: int, status: ThreadStatus):
        self.sessions[sid].update_all_status(status)

    def set_current_tid(self, sid: int, current_tid: int):
        self.sessions[sid].set_current_tid(current_tid)

    def set_current_gthread(self, gtid: int):
        self.selected_gthread = gtid 

    def get_current_gthread(self) -> Optional[int]:
        return self.selected_gthread

    def set_current_session(self, sid: int):
        self.current_session = sid

    def get_current_session(self) -> Optional[int]:
        return self.current_session

    def get_gtid(self, sid: int, tid: int) -> int:
        with self.lock:
            return self.sidtid_to_gtid[(sid, tid)]

    def get_readable_tid_by_gtid(self, gtid: int) -> str:
        with self.lock:
            sid, tid = self.gtid_to_sidtid[gtid]
            return self.get_readable_gtid(sid, tid)

    def get_readable_gtid(self, sid: int, tid: int) -> str:
        # returns something like "1.2"
        # where 1 is global inferior id and 2 is local thread id
        with self.lock:
            giid = self.sidtgid_to_giid[(sid, self.sessions[sid].t_to_tg[tid])]
            return f"{giid}.{self.sessions[sid].tid_to_per_inferior_tid[tid]}"            

    def get_giid(self, sid: int, tgid: str) -> int:
        with self.lock:
            return self.sidtgid_to_giid[(sid, tgid)]
    
    def get_sidtid_by_gtid(self, gtid: int) -> Tuple[int, int]:
        with self.lock:
            return self.gtid_to_sidtid[gtid]

    def get_readable_giid(self, sid: int, tgid: str) -> str:
        with self.lock:
            return str(self.get_giid(sid, tgid))

    def __str__(self) -> str:
        out = "**** SESSION MANAGER START ****\n"
        out += f"- STATE MANAGER META\n"
        out += f"current session: {self.current_session}\n"
        out += f"sidtid_to_gtid: {self.sidtid_to_gtid}\n"
        out += f"gtid_to_sidtid: {self.gtid_to_sidtid}\n"
        out += f"sidtgid_to_giid: {self.sidtgid_to_giid}\n"
        out += f"giid_to_sidtgid: {self.giid_to_sidtgid}\n\n"
        out += f"- SESSION META\n{self.get_all_session_meta()}\n"
        out += "**** SESSION MANAGER END ****\n"
        return out

    def get_all_session_meta(self) -> str:
        out = ""
        for sid in self.sessions:
            out += f"{str(self.sessions[sid])}\n"
        return out

# Eager instantiation
_ = StateManager.inst()
