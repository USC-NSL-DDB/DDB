import asyncio
import threading

class EventLoopThread(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.loop = asyncio.new_event_loop()
        self.futures = {}

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.loop.run_forever()

    def get_loop(self):
        return self.loop