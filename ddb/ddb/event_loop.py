import asyncio
import threading
from typing import Optional

class EventLoopThread(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.loop = asyncio.new_event_loop()
        self.futures = {}
        self.runnning = False

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.runnning = True
        self.loop.run_forever()

    def get_loop(self):
        return self.loop

class GlobalRunningLoop:
    _instance: Optional["GlobalRunningLoop"] = None

    # def __new__(cls, *args, **kwargs):
    #     if not cls._instance:
    #         cls._instance = super(GlobalRunningLoop, cls).__new__(cls, *args, **kwargs)
    #     return cls._instance

    def __init__(self) -> None:
        self._loop = EventLoopThread()
        threading.Thread(target=self._loop.run, args=()).start()

    @staticmethod
    def inst() -> "GlobalRunningLoop":
        if not GlobalRunningLoop._instance:
            GlobalRunningLoop._instance = GlobalRunningLoop()
        return GlobalRunningLoop._instance

    def get_loop(self):
        return self._loop.get_loop()

_ = GlobalRunningLoop.inst()

