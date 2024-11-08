from typing import Deque, Dict
from enum import Enum

from iddb.logging import logger

class DeadlockDetector:
    # class DepedencyType(Enum):
    #     LOCK = "lock"
    #     CALL = "call"

    def __init__(self):
        self.wait_for: Dict[str, str] = {}
        self.lock_owners: Dict[str, str] = {}
        self.start_thrd = None

    def add_data(self, session_tag: str, data: dict):
        """ Add data to the detector
        @param session_tag: the tag of the session that the data belongs to
        @param data: the data to be added to the detector. It should be consistent with that data format.
        """
        thread_data =  data["thread_info"]
        lock_data = data["lock_info"]
        for td in thread_data:
            tid = td["tid"]
            local_wait = td["wait"]
            if len(local_wait) > 1:
                logger.debug(f"thread {tid} is waiting for multiple locks")
                logger.debug(f"local_wait: {local_wait}")
            for w in local_wait:
                self.wait_for[f"{session_tag}:{tid}"] = {
                    "type": int(w["type"]),
                    "id": f"{session_tag}:{w['id']}"
                }
        for ld in lock_data:
            lock_id = ld["lid"]
            owner_tid = ld["owner_tid"]
            self.lock_owners[f"{session_tag}:{lock_id}"] = f"{session_tag}:{owner_tid}"

    def set_starting_thread(self, thrd_tag: str):
        self.start_thrd = thrd_tag

    def add_call_chain(self, chain: Deque[str]):
        """ Add a call chain to the detector
        @param chain: a deque containing the call chain. The first (bottom) element should be the last invoked function.
        """
        if len(chain) < 2:
            return
        caller = chain.pop()
        while len(chain) > 0:
            callee = chain.pop()
            if caller in self.wait_for:
                logger.debug(f"detection wait-relation already exists. Existing {self.wait_for[caller]}")
            self.wait_for[caller] = {
                "type": 2,
                "id": callee
            }
            caller = callee
        self.set_starting_thread(caller)

    def __run_cycle_detection(self, visited: Dict[str, bool], thrd_tag: str) -> bool:
        if thrd_tag in visited:
            return True
        visited[thrd_tag] = True
        if thrd_tag not in self.wait_for:
            return False
        next_wait = self.wait_for[thrd_tag]
        if next_wait["type"] == 1:
            # a wait dependency
            owner_tid = self.lock_owners[next_wait["id"]]
            return self.__run_cycle_detection(visited, owner_tid)
        elif next_wait["type"] == 2:
            # a call dependency
            return self.__run_cycle_detection(visited, next_wait["id"])
        else:
            return False

    def detect(self) -> bool:
        visited = {}

        if self.start_thrd is None:
            self.start_thrd = list(self.wait_for.keys())[0]

        return self.__run_cycle_detection(visited, self.start_thrd)
