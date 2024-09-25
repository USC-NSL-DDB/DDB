import asyncio
from dataclasses import dataclass
import threading
from typing import List, Optional, Set, Tuple
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

@dataclass
class ThreadContext:
    thread_id:int
    ctx: dict[str, int]
    # sp: int
    # pc: int
    # fp: int
    # lr: int

class SessionMeta:
    def __init__(self, sid: int, tag: str, session: "GdbSession") -> None:
        self.tag = tag
        self.sid = sid
        self.current_tid: Optional[int] = None
        self.t_status: dict[int, ThreadStatus] = {}
        self.current_context:ThreadContext=None
        self.in_custom_context = False
        self.session_obj = session
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

    def remove_thread_group(self, tgid: str) -> Set[int]:
        with self.rlock:
            associated_threads = self.tg_to_t[tgid]
            for t in associated_threads:
                del self.t_to_tg[t]
                del self.t_status[t]
                del self.tid_to_per_inferior_tid[t]
                
            del self.tg_to_t[tgid]
            del self.tg_status[tgid]
            del self.tg_to_pid[tgid]
            return associated_threads

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
        out += f"\n\tthread group status: {self.tg_status}"
        out += f"\n\ttid to per thread group tid: {self.tid_to_per_inferior_tid}"
        return out

# A simple wrapper around counter in case any customization later
class GlobalInferiorIdCounter:
    _c: "GlobalInferiorIdCounter" = None
    _lock = Lock()
    
    def __init__(self) -> None:
        from iddb.counter import TSCounter
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
        from iddb.counter import TSCounter
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

    def get_session_meta(self, sid: int) -> Optional[SessionMeta]:
        return self.sessions.get(sid, None)

    def register_session(self, sid: int, tag: str, session: "GdbSession"):
        self.sessions[sid] = SessionMeta(sid, tag, session)

    def get_gtids_by_sid(self, sid: int) -> List[int]:
        with self.lock:
            results = []
            for k, v in self.sidtid_to_gtid.items():
                if k[0] == sid:
                    results.append(v)
            return results

    def add_thread_group(self, sid: int, tgid: str) -> int:
        """
        Adds a thread group (process) to the state manager.

        Args:
            sid (int): The session ID.
            tgid (str): The thread group ID.

        Returns:
            int: The global inferior/process/thread group ID assigned to the thread group.
        """
        with self.lock:
            giid = GlobalInferiorIdCounter.get()
            self.sidtgid_to_giid[(sid, tgid)] = giid
            self.giid_to_sidtgid[giid] = (sid, tgid)

        self.sessions[sid].add_thread_group(tgid)
        return giid

    def remove_thread_group(self, sid: int, tgid: str) -> int:
        """
        Removes a thread group (process) from the state manager.

        Args:
            sid (int): The session ID.
            tgid (str): The thread group ID.

        Returns:
            int: The global inferior/process/thread group ID assigned to the thread group.
        """
        giid = self.sidtgid_to_giid[(sid, tgid)]
            
        local_tids = self.sessions[sid].remove_thread_group(tgid)
        for l_tid in local_tids:
            gtid = self.sidtid_to_gtid[(sid, l_tid)]
            del self.sidtid_to_gtid[(sid, l_tid)]
            del self.gtid_to_sidtid[gtid]
        del self.sidtgid_to_giid[(sid, tgid)]
        del self.giid_to_sidtgid[giid]
        return giid

    def start_thread_group(self, sid: int, tgid: str, pid: int) -> int:
        """
        Update a thread group metadata to "RUNNING" within a session.

        Args:
            sid (int): The session ID.
            tgid (str): The thread group ID.
            pid (int): The process ID.

        Returns:
            int: The corresponding global thread group id.
        """
        self.sessions[sid].start_thread_group(tgid, pid)
        return self.sidtgid_to_giid[(sid, tgid)]
    
    def exit_thread_group(self, sid: int, tgid: str) -> int:
        """
        Exit a thread group.

        Args:
            sid (int): The session ID.
            tgid (str): The thread group ID.

        Returns:
            int: The correspondling global thread group id.
        """
        self.sessions[sid].exit_thread_group(tgid)
        return self.sidtgid_to_giid[(sid, tgid)]

    def create_thread(self, sid: int, tid: int, tgid: str) -> Tuple[int, int]:
        """
        Creates a new global thread in the state manager by mapping the session specific thread information.

        Args:
            sid (int): The session ID from gdb/mi output.
            tid (int): The thread ID from gdb/mi output.
            tgid (str): The thread group ID from gdb/mi output.

        Returns:
            int: The global thread ID assigned to the new thread.
            int: The global thread group id associated with this newly created thread.
        """
        with self.lock:
            gtid = GlobalThreadIdCounter.get()
            self.sidtid_to_gtid[(sid, tid)] = gtid
            self.gtid_to_sidtid[gtid] = (sid, tid)
        self.sessions[sid].create_thread(tid, tgid)
        giid = self.sidtgid_to_giid[(sid, tgid)]
        return (gtid, giid)

    def update_thread_status(self, sid: int, tid: int, status: ThreadStatus):
        # if status == ThreadStatus.STOPPED:
        #     self.sessions[sid].resolve_stop_event()
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
            if not (sid, tid) in self.sidtid_to_gtid:
                return -1
            return self.sidtid_to_gtid[(sid, tid)]
    def remove_thread(self, sid: int, tid: int) -> int:
        with self.lock:
            gtid = self.sidtid_to_gtid[(sid, tid)]
            del self.sidtid_to_gtid[(sid, tid)]
            del self.gtid_to_sidtid[gtid]
        return gtid

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
    def get_session_by_tag(self,tag:str)->int:
        for sid in self.sessions:
            if self.sessions[sid].tag == tag:
                return sid
        return -1
# Eager instantiation
_ = StateManager.inst()
