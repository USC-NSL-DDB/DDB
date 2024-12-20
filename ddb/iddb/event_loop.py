import asyncio
import threading
from typing import Optional
import threading

class EventLoopThread(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.loop: asyncio.AbstractEventLoop = asyncio.new_event_loop()
        self.futures = {}
        self.runnning = False
        self._stop_event = threading.Event()  # Used to signal the thread to stop

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.runnning = True
        try:
            self.loop.run_forever()
        finally:
            self.loop.close()  # Ensure the loop is properly closed when the thread exits
            self.running = False

    def get_loop(self):
        return self.loop

    def stop(self):
        """Stop the event loop and wait for the thread to finish."""
        if self.running:
            for task in asyncio.all_tasks(self.loop):
                task.cancel()
            self.loop.call_soon_threadsafe(self.loop.stop)  # Signal the loop to stop
            self._stop_event.set()  # Signal the thread to exit
        self.join()  # Wait for the thread to finish

class GlobalRunningLoop:
    _instance: Optional["GlobalRunningLoop"] = None

    def __new__(cls):
        if not cls._instance:
            cls._instance = super(GlobalRunningLoop, cls).__new__(cls)
            cls._instance._loop = EventLoopThread()
            cls._instance._loop.loop.set_debug(False)
            cls._instance._loop.start()
        return cls._instance

    def get_loop(self) -> asyncio.AbstractEventLoop:
        return self._loop.get_loop()

    def stop(self):
        """Stop the global event loop."""
        self._loop.stop()

class AsyncSSHLoopThread(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.loop: asyncio.AbstractEventLoop = asyncio.new_event_loop()
        self.runnning = False
        self._stop_event = threading.Event()  # Used to signal the thread to stop

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.runnning = True
        try:
            self.loop.run_forever()
        finally:
            self.loop.close()  # Ensure the loop is properly closed when the thread exits
            self.running = False

    def get_loop(self):
        return self.loop

    def stop(self):
        """Stop the event loop and wait for the thread to finish."""
        if self.running:
            for task in asyncio.all_tasks(self.loop):
                task.cancel()
            self.loop.call_soon_threadsafe(self.loop.stop)  # Signal the loop to stop
            self._stop_event.set()  # Signal the thread to exit
        self.join()  # Wait for the thread to finish

class AsyncSSHLoop:
    _instance: Optional["AsyncSSHLoop"] = None

    def __new__(cls):
        if not cls._instance:
            cls._instance = super(AsyncSSHLoop, cls).__new__(cls)
            cls._instance._loop = AsyncSSHLoopThread()
            cls._instance._loop.loop.set_debug(False)
            cls._instance._loop.start()
        return cls._instance

    def get_loop(self) -> asyncio.AbstractEventLoop:
        return self._loop.get_loop()

    def stop(self):
        """Stop the global event loop."""
        self._loop.stop()

class AsyncSSHConnLoopThread(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.loop: asyncio.AbstractEventLoop = asyncio.new_event_loop()
        self.runnning = False
        self._stop_event = threading.Event()  # Used to signal the thread to stop

    def run(self):
        asyncio.set_event_loop(self.loop)
        self.runnning = True
        try:
            self.loop.run_forever()
        finally:
            self.loop.close()  # Ensure the loop is properly closed when the thread exits
            self.running = False

    def get_loop(self):
        return self.loop

    def stop(self):
        """Stop the event loop and wait for the thread to finish."""
        if self.running:
            for task in asyncio.all_tasks(self.loop):
                task.cancel()
            self.loop.call_soon_threadsafe(self.loop.stop)  # Signal the loop to stop
            self._stop_event.set()  # Signal the thread to exit
        self.join()  # Wait for the thread to finish

class AsyncSSHConnLoop:
    _instance: Optional["AsyncSSHConnLoop"] = None

    def __new__(cls):
        if not cls._instance:
            cls._instance = super(AsyncSSHConnLoop, cls).__new__(cls)
            cls._instance._loop = AsyncSSHConnLoopThread()
            cls._instance._loop.loop.set_debug(False)
            cls._instance._loop.start()
        return cls._instance

    def get_loop(self) -> asyncio.AbstractEventLoop:
        return self._loop.get_loop()

    def stop(self):
        """Stop the global event loop."""
        self._loop.stop()
