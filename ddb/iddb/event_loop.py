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

    def __new__(cls):
        if not cls._instance:
            cls._instance = super(GlobalRunningLoop, cls).__new__(cls)
            cls._instance._loop = EventLoopThread()
            cls._instance._loop.loop.set_debug(True)
            cls._instance._loop.start()
        return cls._instance

    def get_loop(self):
        return self._loop.get_loop()

# GlobalRunningLoop()
